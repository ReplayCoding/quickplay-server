mod configuration;
mod io_util;
mod net;
mod quickplay;

use std::{
    collections::HashMap, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc, time::Duration,
};

use argh::FromArgs;
use configuration::Configuration;
use net::message::{Message, MessageDisconnect};
use net::netchannel2::NetChannel2;
use net::packet::{decode_raw_packet, Packet};
use quickplay::global::QuickplayGlobal;
use quickplay::session::QuickplaySession;
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{error::TryRecvError, UnboundedReceiver, UnboundedSender},
        RwLock,
    },
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

struct Connection {
    socket: Arc<UdpSocket>,
    client_addr: SocketAddr,
    packet_receiver: UnboundedReceiver<Vec<u8>>,
    netchan: NetChannel2,
    cancel_token: CancellationToken,

    quickplay: QuickplaySession,

    configuration: &'static Configuration,
}

impl Connection {
    fn new(
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        netchan: NetChannel2,
        quickplay: Arc<QuickplayGlobal>,
        configuration: &'static Configuration,
    ) -> (CancellationToken, UnboundedSender<Vec<u8>>) {
        let cancel_token = CancellationToken::new();

        let (packet_sender, packet_receiver) = tokio::sync::mpsc::unbounded_channel();

        let self_ = Self {
            socket,
            client_addr,
            packet_receiver,
            netchan,
            cancel_token: cancel_token.clone(),

            quickplay: QuickplaySession::new(quickplay, configuration),

            configuration,
        };

        tokio::task::spawn(self_.background_worker());

        (cancel_token, packet_sender)
    }

    async fn handle_packet(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let (messages, files) = self.netchan.read_packet(data)?;

        for message in messages {
            trace!("got message {:?}", message);
            match message {
                Message::Disconnect(_) => self.cancel_token.cancel(),
                Message::SignonState(message) => {
                    // SIGNONSTATE_CONNECTED = 2
                    if message.signon_state != 2 {
                        debug!("unexpected signon state {}", message.signon_state);
                    }

                    // let message =
                    //     if let Some(destination_server) = { self.quickplay.find_server().await } {
                    //         Message::StringCmd(MessageStringCmd {
                    //             command: format!("redirect {}", destination_server),
                    //         })
                    //     } else {
                    //         Message::Disconnect(MessageDisconnect {
                    //             reason: "No matches found with selected filter".to_string(),
                    //         })
                    //     };

                    for i in 0..10 {
                        let message = Message::Print(net::message::MessagePrint {
                            text: format!("Hi World {i}"),
                        });
                        self.netchan.queue_reliable_messages(&[message])?;
                    }

                    self.netchan.queue_reliable_transfer(
                        net::netchannel2::StreamType::File,
                        net::netchannel2::TransferType::File {
                            transfer_id: 20,
                            filename: "piss.txt".to_string(),
                        },
                        std::fs::read("/home/user/Projects/tf2_stuff/server/archive.zip").unwrap(),
                    )?;
                }
                Message::SetConVars(message) => {
                    if let Err(error_message) = self
                        .quickplay
                        .update_preferences_from_convars(&message.convars)
                    {
                        self.netchan.queue_reliable_messages(&[Message::Disconnect(
                            MessageDisconnect {
                                reason: error_message,
                            },
                        )])?;
                    };
                }

                _ => debug!("received unhandled message: {:?}", message),
            }
        }

        for file in files {
            std::fs::write(file.filename, file.data)?;
        }

        Ok(())
    }

    async fn background_worker(mut self) {
        while !self.cancel_token.is_cancelled() {
            // process any incoming packets
            match self.packet_receiver.try_recv() {
                Ok(packet) => {
                    if let Err(err) = self.handle_packet(&packet).await {
                        debug!("error occured while handling incoming packet: {err:?}");
                    };
                }

                // If the channel gets disconnected, the task *should* have been
                // cancelled, so just wait until the next iteration to leave the
                // loop
                Err(TryRecvError::Disconnected) | Err(TryRecvError::Empty) => {}
            }

            match self.netchan.write_packet(&[]) {
                Ok(data) => {
                    if let Err(err) = self.socket.send_to(&data, self.client_addr).await {
                        warn!("error occured while sending outgoing data: {:?}", err);
                    };
                }
                Err(err) => {
                    debug!("error occured while creating outgoing packet: {:?}", err);
                }
            };

            tokio::time::sleep(Duration::from_millis(
                self.configuration.server.connection_update_delay,
            ))
            .await;
        }

        trace!("stopping background task for connection");
    }
}

type ConnectionMap =
    Arc<RwLock<HashMap<SocketAddr, (CancellationToken, UnboundedSender<Vec<u8>>)>>>;
struct Server {
    socket: Arc<UdpSocket>,

    connections: ConnectionMap,
    cancel_token: CancellationToken,

    quickplay: Arc<QuickplayGlobal>,

    configuration: &'static Configuration,
}

impl Server {
    async fn new(configuration: &'static Configuration) -> anyhow::Result<Self> {
        let bind_address = SocketAddr::from_str(&configuration.server.bind_address)?;

        let socket = UdpSocket::bind(bind_address).await?;
        info!("bound to address {:?}", socket.local_addr()?);

        let cancel_token = CancellationToken::new();

        let server = Self {
            connections: Arc::new(HashMap::new().into()),
            socket: socket.into(),
            cancel_token,
            quickplay: Arc::new(QuickplayGlobal::new(configuration)),

            configuration,
        };

        Ok(server)
    }

    async fn handle_packet(&self, from: SocketAddr, packet_data: &[u8]) -> anyhow::Result<()> {
        let packet = decode_raw_packet(packet_data)?;

        match packet {
            Packet::Connectionless(data) => {
                if let Some(challenge) = net::connectionless::process_connectionless_packet(
                    &self.socket,
                    from,
                    &data,
                    self.configuration,
                )
                .await?
                {
                    self.create_connection(from, challenge).await;
                };
            }
            Packet::Regular(data) => {
                if let Some((_, packet_receiver)) = self.connections.read().await.get(&from) {
                    packet_receiver.send(data.into_owned())?;
                } else {
                    debug!(
                        "got netchannel message, but no connection with client {:?}",
                        from
                    );
                }
            }
        }

        Ok(())
    }

    async fn create_connection(&self, from: SocketAddr, challenge: u32) {
        let connection = Connection::new(
            self.socket.clone(),
            from,
            NetChannel2::new(challenge),
            self.quickplay.clone(),
            self.configuration,
        );

        tokio::task::spawn(Self::remove_dead_connection(
            connection.0.clone(),
            Duration::from_millis(self.configuration.server.connection_timeout),
            from,
            self.connections.clone(),
        ));
        self.connections.write().await.insert(from, connection);

        debug!("created netchannel for client {:?}", from);
    }

    async fn remove_dead_connection(
        cancel_token: CancellationToken,
        timeout: Duration,
        client_address: SocketAddr,
        connections: ConnectionMap,
    ) {
        tokio::select! {
            _ = tokio::time::sleep(timeout) => {
                // The connection has lived for too long, tell the worker to stop
                cancel_token.cancel();
            }

            _ = cancel_token.cancelled() => {}
        }

        connections.write().await.remove(&client_address);
        debug!("removed dead connection for {client_address:?}")
    }

    async fn run(&self) {
        let mut packet_data = vec![0u8; u16::MAX.into()];
        loop {
            match self.socket.recv_from(&mut packet_data).await {
                Ok((packet_size, from)) => {
                    let packet_data = &packet_data[..packet_size];

                    if let Err(err) = self.handle_packet(from, packet_data).await {
                        debug!("error occured while handling packet: {:?}", err);
                    }
                }
                Err(err) => {
                    warn!("error occured while receiving packet: {:#?}", err);
                }
            };
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    let configuration = Box::leak(Box::new(configuration));

    let server = Server::new(configuration).await?;
    server.run().await;

    Ok(())
}
