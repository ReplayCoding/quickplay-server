use std::{
    hash::{DefaultHasher, Hash, Hasher},
    io::Cursor,
    net::SocketAddr,
};

use anyhow::anyhow;
use bitstream_io::{BitRead, BitReader, BitWrite, BitWriter, LittleEndian};
use log::trace;

use crate::{io_util::write_string, netchannel::NetChannel, PacketInfo, CONNECTIONLESS_HEADER};

const PROTOCOL_VERSION: u32 = 24;
const AUTH_PROTOCOL_HASHEDCDKEY: u32 = 2;

/// Create an opaque challenge number for an address, which will be consistent for this address
fn get_challenge_for_address(addr: SocketAddr) -> u32 {
    // TODO: evaluate if this hasher fits our needs
    let mut hasher = DefaultHasher::new();
    addr.hash(&mut hasher);

    // intentionally truncating the value
    hasher.finish() as u32
}

fn reject_connection(
    packet: &PacketInfo,
    client_challenge: u32,
    message: &str,
) -> anyhow::Result<()> {
    let mut response_cursor = Cursor::new(Vec::<u8>::new());
    let mut response = BitWriter::endian(&mut response_cursor, LittleEndian);

    response.write_out::<32, _>(CONNECTIONLESS_HEADER)?;
    response.write_out::<8, _>(b'9')?; // S2C_CONNREJECT
    response.write_out::<32, _>(client_challenge)?; // S2C_CONNREJECT
    write_string(&mut response, message)?;

    packet.send(&response_cursor.into_inner())?;

    Ok(())
}

fn handle_a2s_get_challenge<R: BitRead>(packet: &PacketInfo, reader: &mut R) -> anyhow::Result<()> {
    let client_challenge: u32 = reader.read_in::<32, _>()?;
    let server_challenge = get_challenge_for_address(packet.from);

    trace!("got challenge {client_challenge:08x} from client");

    let mut response_cursor = Cursor::new(Vec::<u8>::new());
    let mut response = BitWriter::endian(&mut response_cursor, LittleEndian);

    response.write_out::<32, _>(CONNECTIONLESS_HEADER)?;
    response.write_out::<8, _>(b'A')?; // S2C_CHALLENGE
    response.write_out::<32, _>(0x5a4f_4933)?; // S2C_MAGICVERSION
    response.write_out::<32, _>(server_challenge)?;
    response.write_out::<32, _>(client_challenge)?;
    response.write_out::<32, _>(AUTH_PROTOCOL_HASHEDCDKEY)?;

    packet.send(&response_cursor.into_inner())?;

    Ok(())
}

fn handle_c2s_connect(
    packet: &PacketInfo,
    reader: &mut BitReader<Cursor<&[u8]>, LittleEndian>,
) -> anyhow::Result<NetChannel> {
    let protocol_version: u32 = reader.read_in::<32, _>()?;
    let auth_protocol: u32 = reader.read_in::<32, _>()?;
    let server_challenge: u32 = reader.read_in::<32, _>()?;
    let client_challenge: u32 = reader.read_in::<32, _>()?;

    if server_challenge != get_challenge_for_address(packet.from) {
        reject_connection(packet, client_challenge, "#GameUI_ServerRejectBadChallenge")?;
        return Err(anyhow!(
            "mismatched server challenge: {} != {}",
            server_challenge,
            get_challenge_for_address(packet.from)
        ));
    };

    if protocol_version != PROTOCOL_VERSION {
        reject_connection(packet, client_challenge, "Unexpected protocol version")?;
        return Err(anyhow!("unexpected protocl version: {protocol_version}"));
    }

    if auth_protocol != AUTH_PROTOCOL_HASHEDCDKEY {
        reject_connection(
            packet,
            client_challenge,
            "unexpected authentication protocol",
        )?;

        return Err(anyhow!(
            "unexpected authentication protocol: {}",
            auth_protocol
        ));
    }

    // Unused data, no need to read it
    // let name = read_string(reader, 256)?;
    // let password = read_string(reader, 256)?;
    // let product_version = read_string(reader, 32)?;
    // let cdkey = read_string(reader, 2048)?;

    // Everything is correct, tell the client to switch over to netchannels
    let mut response_cursor = Cursor::new(Vec::<u8>::new());
    let mut response = BitWriter::endian(&mut response_cursor, LittleEndian);

    response.write_out::<32, _>(CONNECTIONLESS_HEADER)?;
    response.write_out::<8, _>(b'B')?; // S2C_CONNECTION
    response.write_out::<32, _>(client_challenge)?;
    write_string(&mut response, "0000000000")?; // padding

    packet.send(&response_cursor.into_inner())?;

    Ok(NetChannel::new(server_challenge))
}

pub fn process_connectionless_packet(packet: &PacketInfo) -> anyhow::Result<Option<NetChannel>> {
    // Cut off connectionless header
    let packet_data = packet
        .data
        .get(4..)
        .ok_or_else(|| anyhow!("connectionless packet doesn't have any data"))?;

    let mut reader = BitReader::endian(Cursor::new(packet_data), LittleEndian);

    let command: u8 = reader.read_in::<8, _>()?;

    match command {
        // A2S_GETCHALLENGE
        b'q' => handle_a2s_get_challenge(packet, &mut reader)?,
        // C2S_CONNECT
        b'k' => return Ok(Some(handle_c2s_connect(packet, &mut reader)?)),
        _ => {
            return Err(anyhow!(
                "unhandled connectionless packet type: {} ({:?})",
                command,
                command as char
            ))
        }
    };

    Ok(None)
}
