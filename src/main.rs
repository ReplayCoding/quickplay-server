mod configuration;
mod io;
mod net;
mod quickplay;

use std::net::UdpSocket;
use std::ops::ControlFlow;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc};

use argh::FromArgs;
use configuration::Configuration;
use net::message::MessageSide;
use net::netchannel::NetChannel;
use net::netmessage::{Disconnect, NetMessage, SetConVars, SignonState, StringCmd};
use net::server::{Connection, ConnectionSetupInfo, Server};
use quickplay::global::QuickplayGlobal;
use quickplay::session::QuickplaySession;

use tracing::debug;
pub use tracing::{info, trace, warn};

struct QuickplayConnection {
    socket: Arc<UdpSocket>,
    client_addr: SocketAddr,
    packet_receiver: Receiver<Vec<u8>>,
    netchannel: NetChannel,

    quickplay: QuickplaySession,

    creation_time: Instant,
    update_interval: Duration,
    timeout: Duration,
}

impl Connection for QuickplayConnection {
    fn run(&mut self) {
        loop {
            match self.packet_receiver.recv_timeout(self.update_interval) {
                Ok(packet) => match self.netchannel.read_packet(&packet) {
                    Ok((messages, _files)) => {
                        for message in messages {
                            match self.handle_message(&message) {
                                Ok(ControlFlow::Continue(_)) => {}
                                // Function has asked to terminate the connection
                                Ok(ControlFlow::Break(_)) => return,
                                Err(err) => trace!("error while handling message {err}"),
                            };
                        }
                    }
                    Err(err) => trace!("error while decoding messages: {err}"),
                },
                // No packets have been received, continue so that we have a
                // chance to send queued messages.
                Err(RecvTimeoutError::Timeout) => {}
                // The channel has been disconnected, terminate the connection.
                Err(RecvTimeoutError::Disconnected) => return,
            };

            if Instant::now().duration_since(self.creation_time) > self.timeout {
                trace!("connection has lived too long, destroying");
                return;
            }

            match self.netchannel.write_packet(&[]) {
                Ok(response) => {
                    if let Err(err) = self.socket.send_to(&response, self.client_addr) {
                        debug!("error while sending response packet: {err}");
                    }
                }
                Err(err) => {
                    debug!("error while creating response packet: {err}");
                }
            };
        }
    }
}

impl QuickplayConnection {
    fn handle_message(&mut self, message: &NetMessage) -> anyhow::Result<ControlFlow<()>> {
        match message {
            NetMessage::Disconnect(_) => return Ok(ControlFlow::Break(())),
            NetMessage::SetConVars(message) => self.handle_set_convars(message)?,
            NetMessage::SignonState(message) => self.handle_signon_state(message)?,
            _ => {}
        }

        Ok(ControlFlow::Continue(()))
    }

    fn handle_set_convars(&mut self, message: &SetConVars) -> anyhow::Result<()> {
        for convar in &message.convars {
            if let Err(err) = self
                .quickplay
                .update_preferences_from_convar(&convar.name, &convar.value)
            {
                self.netchannel
                    .queue_reliable_messages(&[NetMessage::Disconnect(Disconnect {
                        reason: err,
                    })])?;
                return Ok(());
            }
        }

        Ok(())
    }

    fn handle_signon_state(&mut self, message: &SignonState) -> anyhow::Result<()> {
        if message.signon_state != 2 {
            trace!("got unexpected signon state: {}", message.signon_state);
            return Ok(());
        }

        let response = match self.quickplay.find_server() {
            Some(destination_server) => NetMessage::StringCmd(StringCmd {
                command: format!("redirect {destination_server}"),
            }),
            None => NetMessage::Disconnect(Disconnect {
                reason: "No matches found with selected filter".to_string(),
            }),
        };
        self.netchannel.queue_reliable_messages(&[response])?;

        Ok(())
    }
}

fn quickplay_connection_factory(
    setup_info: ConnectionSetupInfo,
    quickplay: Arc<QuickplayGlobal>,
    configuration: &'static Configuration,
) -> Box<dyn Connection> {
    Box::new(QuickplayConnection {
        socket: setup_info.socket,
        client_addr: setup_info.client_addr,
        packet_receiver: setup_info.incoming_packets,
        netchannel: NetChannel::new(MessageSide::Server, setup_info.challenge),
        quickplay: QuickplaySession::new(quickplay, configuration),
        creation_time: Instant::now(),
        update_interval: Duration::from_millis(configuration.server.connection_update_delay),
        timeout: Duration::from_millis(configuration.server.connection_timeout),
    })
}

#[derive(FromArgs)]
#[argh(description = "Quickplay Server")]
struct Arguments {
    /// the path to the configuration to load
    #[argh(option)]
    configuration: Option<PathBuf>,

    /// dump the current configuration to standard output
    #[argh(switch)]
    dump_configuration: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args: Arguments = argh::from_env();

    let configuration = if let Some(configuration) = &args.configuration {
        Configuration::load_from_file(configuration)?
    } else {
        warn!("no configuration provided, using defaults");
        Configuration::load_default()
    };

    if args.dump_configuration {
        println!("{}", toml::to_string_pretty(&configuration)?);
        return Ok(());
    }

    let configuration: &'static Configuration = Box::leak(Box::new(configuration));

    let bind_address = SocketAddr::from_str(&configuration.server.bind_address)?;

    let quickplay = Arc::new(QuickplayGlobal::new(configuration));
    let quickplay_connection_factory = move |setup_info| {
        quickplay_connection_factory(setup_info, quickplay.clone(), configuration)
    };

    let mut server = Server::new(bind_address, Box::new(quickplay_connection_factory))?;
    server.run();

    Ok(())
}
