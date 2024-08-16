mod connectionless;
mod io_util;
mod message;
mod netchannel;

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    net::{SocketAddr, UdpSocket},
    rc::Rc,
};

use anyhow::anyhow;
use message::{Message, MessagePrint};
use netchannel::NetChannel;
use tracing::{debug, info, span, warn};

const CONNECTIONLESS_HEADER: u32 = -1_i32 as u32;
const SPLITPACKET_HEADER: u32 = -2_i32 as u32;
const COMPRESSEDPACKET_HEADER: u32 = -3_i32 as u32;

const COMPRESSION_SNAPPY: &[u8] = b"SNAP";

struct Connection {
    socket: Rc<UdpSocket>,
    client_addr: SocketAddr,
    netchan: RefCell<NetChannel>,

    state: RefCell<ConnectionState>,
}

impl Connection {
    fn new(socket: Rc<UdpSocket>, client_addr: SocketAddr, netchan: NetChannel) -> Self {
        Self {
            socket,
            client_addr,
            netchan: netchan.into(),

            state: ConnectionState::Init.into(),
        }
    }

    fn handle_packet(&self, data: &[u8]) -> anyhow::Result<()> {
        self.netchan
            .borrow_mut()
            .process_packet(data, &mut |message| {
                self.state.borrow_mut().handle_message(message)
            })?;

        tracing::trace!("aaaaa");
        self.netchan.borrow_mut().send_packet(
            &self.socket,
            self.client_addr,
            &[Message::Print(MessagePrint {
                text: format!("hi {:?}\n", std::time::Instant::now()),
            })],
        )?;

        Ok(())
    }
}

enum ConnectionState {
    Init,
}

impl ConnectionState {
    fn handle_message(&mut self, message: &Message) -> anyhow::Result<()> {
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

fn process_packet(
    connections: &mut HashMap<SocketAddr, Connection>,
    socket: Rc<UdpSocket>,
    from: SocketAddr,
    packet_data: &[u8],
) -> anyhow::Result<()> {
    let header_flags = packet_data
        .get(0..4)
        .ok_or_else(|| anyhow!("couldn't get header flags"))?;

    if header_flags == CONNECTIONLESS_HEADER.to_le_bytes() {
        if let Some(challenge) =
            connectionless::process_connectionless_packet(&socket, from, &packet_data)?
        {
            let connection = Connection::new(socket, from, NetChannel::new(challenge));
            debug!("created netchannel for client {:?}", from);
            connections.insert(from, connection);
        };
    } else if let Some(connection) = connections.get_mut(&from) {
        // It's pretty expensive to do this, so only decode when we have a
        // connection
        let packet_data = decode_raw_packet(packet_data)?;

        connection.handle_packet(&packet_data)?;
    } else {
        return Err(anyhow!(
            "got netchannel message, but no connection with client"
        ));
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let socket = Rc::new(UdpSocket::bind("127.0.0.2:4444")?);
    info!("bound to address {:?}", socket.local_addr()?);

    let mut connections = HashMap::new();

    let mut packet_data = vec![0u8; u16::MAX.into()];
    loop {
        let (packet_size, from) = socket.recv_from(&mut packet_data)?;
        let packet_data = &packet_data[..packet_size];

        let mut address_hasher = DefaultHasher::new();
        from.hash(&mut address_hasher);

        let _span = span!(
            tracing::Level::ERROR,
            "handle packet",
            "{:016x}",
            address_hasher.finish(),
        );

        _span.in_scope(|| {
            if let Err(err) = process_packet(&mut connections, socket.clone(), from, packet_data) {
                warn!("error occured while handling packet: {:?}", err);
            };
        });
    }
}
