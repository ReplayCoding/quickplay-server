// TODO: this file needs to be cleaned up
mod connectionless;
mod io_util;
mod netchannel;

use std::{
    borrow::Cow,
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
};

use anyhow::anyhow;
use netchannel::NetChannel;
use tracing::{info, span, trace, warn};

const CONNECTIONLESS_HEADER: u32 = -1_i32 as u32;
const SPLITPACKET_HEADER: u32 = -2_i32 as u32;
const COMPRESSEDPACKET_HEADER: u32 = -3_i32 as u32;

const COMPRESSION_SNAPPY: &[u8] = b"SNAP";

struct PacketInfo<'a> {
    data: &'a [u8],
    from: SocketAddr,
    socket: &'a UdpSocket,
}

impl PacketInfo<'_> {
    /// Send data to the address that this packet originated from
    pub fn send(&self, data: &[u8]) -> std::io::Result<usize> {
        self.socket.send_to(data, self.from)
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
    connections: &mut HashMap<SocketAddr, NetChannel>,
    socket: &UdpSocket,
    from: SocketAddr,
    packet_data: &[u8],
) -> anyhow::Result<()> {
    let packet_data = decode_raw_packet(packet_data)?;

    let header_flags = packet_data
        .get(0..4)
        .ok_or_else(|| anyhow!("couldn't get header flags"))?;

    if header_flags == CONNECTIONLESS_HEADER.to_le_bytes() {
        // TODO: Maybe this struct shouldn't exist
        let packet_info = PacketInfo {
            data: &packet_data,
            from,
            socket,
        };

        if let Some(netchan) = connectionless::process_connectionless_packet(&packet_info)? {
            trace!("created netchannel for client {:?}", from);
            connections.insert(from, netchan);
        };
    } else if let Some(netchan) = connections.get_mut(&from) {
        let _messages = netchan.get_messages(&packet_data)?;
    } else {
        return Err(anyhow!(
            "got netchannel message, but no connection with client"
        ));
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let socket = UdpSocket::bind("127.0.0.2:4444")?;
    info!("bound to address {:?}", socket.local_addr()?);

    let mut connections = HashMap::new();

    let mut packet_data = vec![0u8; u16::MAX.into()];
    loop {
        let (packet_size, addr) = socket.recv_from(&mut packet_data)?;
        let packet_data = &packet_data[..packet_size];

        let _span = span!(tracing::Level::TRACE, "process packet", "{}", addr.to_string());

        _span.in_scope(|| {
            trace!("got packet of size {} from {:?}", packet_size, addr);

            if let Err(err) = process_packet(&mut connections, &socket, addr, packet_data) {
                warn!(
                    "error occured while handling packet from {:?}: {:?}",
                    addr, err
                );
            };
        });
    }
}
