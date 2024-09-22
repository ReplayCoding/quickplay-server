mod configuration;
mod connectionless;
mod io_util;
mod message;
mod netchannel;
mod quickplay;
mod server_list;

use std::{
    borrow::Cow,
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use argh::FromArgs;
use configuration::Configuration;
use message::{Message, MessageDisconnect, MessageStringCmd};
use netchannel::NetChannel;
use quickplay::QuickplaySession;
use server_list::ServerListController;
use tokio::{
    net::UdpSocket,
    sync::{Mutex, RwLock},
    task::JoinSet,
};
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{debug, info, trace, warn};

const CONNECTIONLESS_HEADER: u32 = -1_i32 as u32;
const SPLITPACKET_HEADER: u32 = -2_i32 as u32;
const COMPRESSEDPACKET_HEADER: u32 = -3_i32 as u32;

const COMPRESSION_SNAPPY: &[u8] = b"SNAP";

struct Connection {
    // socket: Arc<UdpSocket>,
    // client_addr: SocketAddr,
    netchan: Arc<Mutex<NetChannel>>,

    created_at: Instant,

    marked_for_death: bool,
    _cancel_guard: DropGuard,

    quickplay: QuickplaySession,
    server_list: Arc<ServerListController>,

    configuration: &'static Configuration,
}

impl Connection {
    fn new(
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        netchan: NetChannel,
        server_list: Arc<ServerListController>,
        configuration: &'static Configuration,
    ) -> Self {
        let netchan = Arc::new(Mutex::new(netchan));
        let cancel_token = CancellationToken::new();

        tokio::task::spawn(Self::send_background(
            socket.clone(),
            client_addr,
            netchan.clone(),
            cancel_token.child_token(),
            configuration,
        ));

        Self {
            // socket,
            // client_addr,
            netchan,

            created_at: Instant::now(),

            marked_for_death: false,
            _cancel_guard: cancel_token.drop_guard(),

            quickplay: QuickplaySession::new(configuration),
            server_list,

            configuration,
        }
    }

    async fn handle_packet(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let messages = self.netchan.lock().await.process_packet(data)?;

        for message in messages {
            trace!("got message {:?}", message);
            match message {
                Message::Disconnect(_) => self.marked_for_death = true,
                Message::SignonState(message) => {
                    // SIGNONSTATE_CONNECTED = 2
                    if message.signon_state != 2 {
                        debug!("unexpected signon state {}", message.signon_state);
                    }

                    let server_list = self.server_list.list().await;

                    let start = Instant::now();
                    let message = if let Some(destination_server) =
                        { self.quickplay.find_server(&server_list) }
                    {
                        Message::StringCmd(MessageStringCmd {
                            command: format!("redirect {}", destination_server),
                        })
                    } else {
                        Message::Disconnect(MessageDisconnect {
                            reason: "No matches found with selected filter".to_string(),
                        })
                    };
                    trace!("quickplay search took {:?}", start.elapsed());

                    self.netchan
                        .lock()
                        .await
                        .queue_reliable_messages(&[message])?;
                }
                Message::SetConVars(message) => {
                    if let Err(error_message) = self
                        .quickplay
                        .update_preferences_from_convars(&message.convars)
                    {
                        self.netchan.lock().await.queue_reliable_messages(&[
                            Message::Disconnect(MessageDisconnect {
                                reason: error_message,
                            }),
                        ])?;
                    };
                }

                _ => debug!("received unhandled message: {:?}", message),
            }
        }

        Ok(())
    }

    async fn send_background(
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        netchan: Arc<Mutex<NetChannel>>,
        cancel_token: CancellationToken,
        configuration: &'static Configuration,
    ) {
        while !cancel_token.is_cancelled() {
            // allow the netchannel to send remaining reliable data
            match netchan.lock().await.create_send_packet(&[]) {
                Ok(data) => {
                    if let Err(err) = socket.send_to(&data, client_addr).await {
                        warn!("error occured while sending outgoing data: {:?}", err);
                    };
                }
                Err(err) => {
                    debug!("error occured while creating outgoing packet: {:?}", err);
                }
            };

            tokio::time::sleep(Duration::from_millis(
                configuration.server.connection_update_delay,
            ))
            .await;
        }

        trace!("stopping background task for connection");
    }
}

fn decompress_packet(packet_data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let compression_type = packet_data
        .get(4..8)
        .ok_or_else(|| anyhow!("couldn't get compression type"))?;

    match compression_type {
        COMPRESSION_SNAPPY => {
            let compressed_data = packet_data
                .get(8..)
                .ok_or_else(|| anyhow!("couldn't get compressed data"))?;

            let mut decoder = snap::raw::Decoder::new();
            Ok(decoder.decompress_vec(compressed_data)?)
        }
        _ => Err(anyhow!("unhandled compression type {compression_type:?}")),
    }
}

fn decode_raw_packet(packet_data: &[u8]) -> anyhow::Result<Cow<'_, [u8]>> {
    let header_flags = packet_data
        .get(0..4)
        .ok_or_else(|| anyhow!("couldn't get header flags"))?;

    if header_flags == SPLITPACKET_HEADER.to_le_bytes() {
        return Err(anyhow!("handle splitpacket"));
    }

    if header_flags == COMPRESSEDPACKET_HEADER.to_le_bytes() {
        let decompressed_packet = decompress_packet(packet_data)?;

        return Ok(Cow::Owned(decompressed_packet));
    }

    Ok(Cow::Borrowed(packet_data))
}

struct Server {
    connections: Arc<RwLock<HashMap<SocketAddr, RwLock<Connection>>>>,
    socket: Arc<UdpSocket>,
    _cancel_guard: DropGuard,

    server_list: Arc<ServerListController>,

    configuration: &'static Configuration,
}

impl Server {
    fn new(socket: UdpSocket, configuration: &'static Configuration) -> anyhow::Result<Self> {
        let connections: Arc<RwLock<HashMap<SocketAddr, RwLock<Connection>>>> =
            Arc::new(HashMap::new().into());

        let cancel_token = CancellationToken::new();
        tokio::spawn(Self::connection_killer(
            connections.clone(),
            cancel_token.child_token(),
            configuration,
        ));

        let server = Self {
            connections: connections,
            socket: socket.into(),
            _cancel_guard: cancel_token.drop_guard(),
            server_list: ServerListController::new(),

            configuration,
        };

        Ok(server)
    }

    async fn connection_killer(
        connections: Arc<RwLock<HashMap<SocketAddr, RwLock<Connection>>>>,
        cancel_token: CancellationToken,
        configuration: &'static Configuration,
    ) {
        while !cancel_token.is_cancelled() {
            let current_time = Instant::now();
            let mut connections_to_kill = vec![];

            for (client_addr, connection) in connections.read().await.iter() {
                let mut connection = connection.write().await;

                if current_time.duration_since(connection.created_at)
                    > Duration::from_millis(configuration.server.connection_timeout)
                {
                    // TODO: maybe we should send a disconnect message before we
                    // kill the connection, so that the client gets some
                    // feedback
                    debug!(
                        "connection for {:?} has lived too long, marked for death",
                        client_addr
                    );
                    connection.marked_for_death = true;
                }

                if connection.marked_for_death {
                    connections_to_kill.push(*client_addr);
                }
            }

            {
                let mut connections = connections.write().await;
                for client_addr in connections_to_kill {
                    debug!("killing connection for {:?}", client_addr);
                    connections.remove(&client_addr);
                }
            }

            tokio::time::sleep(Duration::from_millis(
                configuration.server.connection_update_delay,
            ))
            .await;
        }
    }

    async fn process_packet(&self, from: SocketAddr, packet_data: &[u8]) -> anyhow::Result<()> {
        let header_flags = packet_data
            .get(0..4)
            .ok_or_else(|| anyhow!("couldn't get header flags"))?;

        if header_flags == CONNECTIONLESS_HEADER.to_le_bytes() {
            if let Some(challenge) = connectionless::process_connectionless_packet(
                &self.socket,
                from,
                packet_data,
                self.configuration,
            )
            .await?
            {
                let connection = Connection::new(
                    self.socket.clone(),
                    from,
                    NetChannel::new(challenge, self.configuration),
                    self.server_list.clone(),
                    self.configuration,
                );
                debug!("created netchannel for client {:?}", from);
                self.connections
                    .write()
                    .await
                    .insert(from, connection.into());
            };
        } else if let Some(connection) = self.connections.read().await.get(&from) {
            let mut connection = connection.write().await;

            // It's more expensive to do this, so only decode when we have a
            // connection
            let packet_data = decode_raw_packet(packet_data)?;

            connection.handle_packet(&packet_data).await?;
        } else {
            debug!(
                "got netchannel message, but no connection with client {:?}",
                from
            );
        }

        Ok(())
    }

    async fn receive_packets(&self) {
        let mut packet_data = vec![0u8; u16::MAX.into()];
        loop {
            match self.socket.recv_from(&mut packet_data).await {
                Ok((packet_size, from)) => {
                    let packet_data = &packet_data[..packet_size];

                    if let Err(err) = self.process_packet(from, packet_data).await {
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

    let bind_address = SocketAddr::from_str(&configuration.server.bind_address)?;

    let socket = UdpSocket::bind(bind_address).await?;
    info!("bound to address {:?}", socket.local_addr()?);

    let server = Box::leak(Server::new(socket, configuration)?.into());

    let mut tasks = JoinSet::new();

    for _ in 0..configuration.server.num_packet_tasks {
        tasks.spawn(server.receive_packets());
    }

    tasks.join_all().await;

    Ok(())
}
