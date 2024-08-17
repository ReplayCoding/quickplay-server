mod connectionless;
mod io_util;
mod message;
mod netchannel;

use std::{
    borrow::Cow,
    net::{SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::anyhow;
use dashmap::DashMap;
use message::Message;
use netchannel::NetChannel;
use tracing::{debug, info, trace, warn};

const CONNECTIONLESS_HEADER: u32 = -1_i32 as u32;
const SPLITPACKET_HEADER: u32 = -2_i32 as u32;
const COMPRESSEDPACKET_HEADER: u32 = -3_i32 as u32;

const COMPRESSION_SNAPPY: &[u8] = b"SNAP";

const WATCHDOG_UPDATE_DELAY: Duration = Duration::from_secs(1);
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(10);

struct Connection {
    socket: Arc<UdpSocket>,
    client_addr: SocketAddr,
    netchan: Mutex<NetChannel>,

    state: Mutex<ConnectionState>,

    created_at: Instant,
    marked_for_death: bool,
}

impl Connection {
    fn new(socket: Arc<UdpSocket>, client_addr: SocketAddr, netchan: NetChannel) -> Self {
        Self {
            socket,
            client_addr,
            netchan: netchan.into(),

            state: ConnectionState::Init.into(),
            created_at: Instant::now(),
            marked_for_death: false,
        }
    }

    fn handle_packet(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let mut netchan = self.netchan.lock().expect("couldn't acquire netchan mutex");
        let mut state = self.state.lock().expect("couldn't acquire state mutex");

        netchan.process_packet(data, &mut |message| state.handle_message(message))?;

        // make sure we unlock stuff so that tick can take the locks
        drop(netchan);
        drop(state);

        self.tick(false)?;

        Ok(())
    }

    fn tick(&mut self, send_empty: bool) -> anyhow::Result<()> {
        let mut netchan = self.netchan.lock().expect("couldn't acquire netchan mutex");
        let state = *self.state.lock().expect("couldn't acquire state mutex");

        if state == ConnectionState::Disconnected {
            self.marked_for_death = true;
        }

        if send_empty {
            // dumb testing hack get rid of me
            netchan.send_packet(
                &self.socket,
                self.client_addr,
                &[Message::Print(message::MessagePrint {
                    text: "lol\n".to_string(),
                })],
            )?;

            // allow the netchannel to send remaining reliable data
            netchan.send_packet(&self.socket, self.client_addr, &[])?;
        }

        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ConnectionState {
    Init,
    Disconnected,
}

impl ConnectionState {
    fn handle_message(&mut self, message: &Message) -> anyhow::Result<()> {
        trace!("got message {:?}", message);

        if let Message::Disconnect(_) = message {
            *self = Self::Disconnected;
        }

        Ok(())
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
    connections: Arc<DashMap<SocketAddr, Connection>>,
    socket: Arc<UdpSocket>,
}

impl Server {
    fn new(socket: UdpSocket) -> Self {
        let connections = Arc::new(DashMap::new());

        let server = Self {
            connections: connections.clone(),
            socket: socket.into(),
        };

        std::thread::spawn(move || Self::ticker(connections));

        server
    }

    fn ticker(connections: Arc<DashMap<SocketAddr, Connection>>) {
        loop {
            let current_time = Instant::now();
            let mut connections_to_kill = vec![];

            for mut kv in connections.iter_mut() {
                let (client_addr, connection) = kv.pair_mut();

                if let Err(err) = connection.tick(true) {
                    warn!("error occured while updating connection: {:?}", err);
                };

                if current_time.duration_since(connection.created_at) > WATCHDOG_TIMEOUT {
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

            for client_addr in connections_to_kill {
                debug!("killing connection for {:?}", client_addr);
                connections.remove(&client_addr);
            }

            std::thread::sleep(WATCHDOG_UPDATE_DELAY);
        }
    }

    fn process_packet(&mut self, from: SocketAddr, packet_data: &[u8]) -> anyhow::Result<()> {
        let header_flags = packet_data
            .get(0..4)
            .ok_or_else(|| anyhow!("couldn't get header flags"))?;

        if header_flags == CONNECTIONLESS_HEADER.to_le_bytes() {
            if let Some(challenge) =
                connectionless::process_connectionless_packet(&self.socket, from, packet_data)?
            {
                let connection =
                    Connection::new(self.socket.clone(), from, NetChannel::new(challenge));
                debug!("created netchannel for client {:?}", from);
                self.connections.insert(from, connection);
            };
        } else if let Some(mut kv) = self.connections.get_mut(&from) {
            let connection = kv.value_mut();

            // It's pretty expensive to do this, so only decode when we have a
            // connection
            let packet_data = decode_raw_packet(packet_data)?;

            connection.handle_packet(&packet_data)?;
        } else {
            debug!(
                "got netchannel message, but no connection with client {:?}",
                from
            );
        }

        Ok(())
    }

    fn run(&mut self) -> anyhow::Result<()> {
        let mut packet_data = vec![0u8; u16::MAX.into()];
        loop {
            let (packet_size, from) = self.socket.recv_from(&mut packet_data)?;
            let packet_data = &packet_data[..packet_size];

            if let Err(err) = self.process_packet(from, packet_data) {
                warn!("error occured while handling packet: {:?}", err);
            };
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let socket = UdpSocket::bind("127.0.0.2:4444")?;
    info!("bound to address {:?}", socket.local_addr()?);

    let mut server = Server::new(socket);

    server.run()?;

    Ok(())
}
