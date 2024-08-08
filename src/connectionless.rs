//! Handles the connectionless part of the source engine network protocol
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    io::Cursor,
    net::SocketAddr,
};

use anyhow::anyhow;
use bitstream_io::{BitRead, BitReader, BitWrite, BitWriter, LittleEndian};
use log::trace;

use crate::{PacketInfo, CONNECTIONLESS_HEADER};

const PROTOCOL_HASHEDCDKEY: u32 = 2;

/// Create an opaque challenge number for an address, which will be consistent for this address
fn create_challenge_for_address(addr: SocketAddr) -> u32 {
    let mut hasher = DefaultHasher::new();
    addr.hash(&mut hasher);

    hasher.finish() as u32
}

fn handle_get_challenge<R: BitRead>(packet: &PacketInfo, reader: &mut R) -> anyhow::Result<()> {
    let client_challenge: u32 = reader.read(32)?;
    let server_challenge = create_challenge_for_address(packet.from);

    trace!("got challenge {client_challenge:08x} from client");

    let mut response_cursor = Cursor::new(Vec::<u8>::new());
    let mut response = BitWriter::endian(&mut response_cursor, LittleEndian);

    response.write(32, CONNECTIONLESS_HEADER)?;
    response.write(8, b'A')?; // S2C_CHALLENGE
    response.write(32, 0x5a4f4933)?; // S2C_MAGICVERSION
    response.write(32, server_challenge)?;
    response.write(32, client_challenge)?;
    response.write(32, PROTOCOL_HASHEDCDKEY)?;

    packet.send(&response_cursor.into_inner())?;

    Ok(())
}

pub fn process_connectionless_packet(packet: &PacketInfo) -> anyhow::Result<()> {
    // Cut off connectionless header
    let packet_data = packet
        .data
        .get(4..)
        .ok_or_else(|| anyhow!("connectionless packet doesn't have any data"))?;

    let mut reader = BitReader::endian(Cursor::new(packet_data), LittleEndian);

    let command: u8 = reader.read(8)?;

    match command {
        // A2S_GETCHALLENGE
        b'q' => handle_get_challenge(packet, &mut reader),
        _ => Err(anyhow!(
            "unhandled connectionless packet type: {} ({:?})",
            command,
            command as char
        )),
    }
}
