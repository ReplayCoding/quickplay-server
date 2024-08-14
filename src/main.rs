// TODO: this file needs to be cleaned up
mod connectionless;
mod io_util;
mod message;
mod netchannel;

use std::{
    borrow::Cow,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    net::{SocketAddr, UdpSocket},
};

use anyhow::anyhow;
use message::Message;
use netchannel::NetChannel;
use tracing::{debug, info, span, trace, warn};

const CONNECTIONLESS_HEADER: u32 = -1_i32 as u32;
const SPLITPACKET_HEADER: u32 = -2_i32 as u32;
const COMPRESSEDPACKET_HEADER: u32 = -3_i32 as u32;

const COMPRESSION_SNAPPY: &[u8] = b"SNAP";

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
        if let Some(netchan) =
            connectionless::process_connectionless_packet(socket, from, &packet_data)?
        {
            debug!("created netchannel for client {:?}", from);
            connections.insert(from, netchan);
        };
    } else if let Some(netchan) = connections.get_mut(&from) {
        let mut should_send_print = false;
        netchan.process_packet(&packet_data, &mut |message| {
            debug!("got message {:?}", message);
            if let Message::SignonState(_) = message {
                should_send_print = true;
            }
            Ok(())
        })?;

        if should_send_print {
            debug!("Sending print");
            netchan.queue_unreliable_message(Message::Print(message::MessagePrint {
                text: "TEST TEST TEST 🐸🐸🐸🐸🐸🐸🐸🐸🐸🐸🐸🐸🐸🐸🐸\n".to_string(),
            }));

            netchan.queue_unreliable_message(Message::StringCmd(message::MessageStringCmd {
                command: "redirect 127.0.0.1:27015\n".to_string(),
            }));

            netchan.send_packet(socket, from)?;
        }
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
            trace!("got packet of size {}", packet_size);

            if let Err(err) = process_packet(&mut connections, &socket, from, packet_data) {
                warn!("error occured while handling packet: {:?}", err);
            };
        });
    }
}
