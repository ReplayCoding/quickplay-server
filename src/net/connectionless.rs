use std::{
    hash::{DefaultHasher, Hash, Hasher},
    io::Cursor,
    net::SocketAddr,
};

use anyhow::anyhow;
use bitstream_io::{BitRead, BitReader, BitWrite, BitWriter, LittleEndian};
use tokio::net::UdpSocket;
use tracing::trace;

use crate::{configuration::Configuration, io_util::write_string};

use super::packet::CONNECTIONLESS_HEADER;

const PROTOCOL_VERSION: u8 = 24;
const AUTH_PROTOCOL_HASHEDCDKEY: u32 = 2;

const A2S_INFO_QUERY_STRING: &[u8; 20] = b"Source Engine Query\0";

/// Create an opaque challenge number for an address, which will be consistent for this address
fn get_challenge_for_address(addr: SocketAddr) -> u32 {
    // TODO: evaluate if this hasher fits our needs
    let mut hasher = DefaultHasher::new();
    addr.hash(&mut hasher);

    // intentionally truncating the value
    hasher.finish() as u32
}

async fn reject_connection(
    socket: &UdpSocket,
    from: SocketAddr,
    client_challenge: u32,
    message: &str,
) -> anyhow::Result<()> {
    let mut response_cursor = Cursor::new(Vec::<u8>::new());
    let mut response = BitWriter::endian(&mut response_cursor, LittleEndian);

    response.write_out::<32, _>(CONNECTIONLESS_HEADER)?;
    response.write_out::<8, _>(b'9')?; // S2C_CONNREJECT
    response.write_out::<32, _>(client_challenge)?; // S2C_CONNREJECT
    write_string(&mut response, message)?;

    socket.send_to(&response_cursor.into_inner(), from).await?;

    Ok(())
}

async fn handle_a2s_get_challenge<R: BitRead>(
    socket: &UdpSocket,
    from: SocketAddr,
    reader: &mut R,
) -> anyhow::Result<()> {
    let client_challenge: u32 = reader.read_in::<32, _>()?;
    let server_challenge = get_challenge_for_address(from);

    trace!("got challenge {client_challenge:08x} from client");

    let mut response_cursor = Cursor::new(Vec::<u8>::new());
    let mut response = BitWriter::endian(&mut response_cursor, LittleEndian);

    response.write_out::<32, _>(CONNECTIONLESS_HEADER)?;
    response.write_out::<8, _>(b'A')?; // S2C_CHALLENGE
    response.write_out::<32, _>(0x5a4f_4933)?; // S2C_MAGICVERSION
    response.write_out::<32, _>(server_challenge)?;
    response.write_out::<32, _>(client_challenge)?;
    response.write_out::<32, _>(AUTH_PROTOCOL_HASHEDCDKEY)?;

    socket.send_to(&response_cursor.into_inner(), from).await?;

    Ok(())
}

async fn handle_c2s_connect(
    socket: &UdpSocket,
    from: SocketAddr,
    reader: &mut BitReader<Cursor<&[u8]>, LittleEndian>,
) -> anyhow::Result<u32> {
    let protocol_version: u32 = reader.read_in::<32, _>()?;
    let auth_protocol: u32 = reader.read_in::<32, _>()?;
    let server_challenge: u32 = reader.read_in::<32, _>()?;
    let client_challenge: u32 = reader.read_in::<32, _>()?;

    if server_challenge != get_challenge_for_address(from) {
        reject_connection(
            socket,
            from,
            client_challenge,
            "#GameUI_ServerRejectBadChallenge",
        )
        .await?;
        return Err(anyhow!(
            "mismatched server challenge: {} != {}",
            server_challenge,
            get_challenge_for_address(from)
        ));
    };

    if protocol_version != u32::from(PROTOCOL_VERSION) {
        reject_connection(
            socket,
            from,
            client_challenge,
            "Unexpected protocol version",
        )
        .await?;
        return Err(anyhow!("unexpected protocl version: {protocol_version}"));
    }

    if auth_protocol != AUTH_PROTOCOL_HASHEDCDKEY {
        reject_connection(
            socket,
            from,
            client_challenge,
            "unexpected authentication protocol",
        )
        .await?;

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

    socket.send_to(&response_cursor.into_inner(), from).await?;

    Ok(server_challenge)
}

async fn handle_a2s_info(
    socket: &UdpSocket,
    from: SocketAddr,
    data: &[u8],
    configuration: &Configuration,
) -> anyhow::Result<()> {
    // The packet always has the query string, but won't have a challenge value
    // unless we send one back. So if the entire message is the query string, we
    // can send a challenge.
    if data == A2S_INFO_QUERY_STRING {
        let mut response_cursor = Cursor::new(Vec::<u8>::new());
        let mut response = BitWriter::endian(&mut response_cursor, LittleEndian);

        // See the documentation for A2S_SERVERQUERY_GETCHALLENGE on the VDC wiki.
        response.write_out::<32, _>(CONNECTIONLESS_HEADER)?;
        response.write_out::<8, _>(b'A')?; // S2C_CHALLENGE
        response.write_out::<32, _>(get_challenge_for_address(from))?;

        socket.send_to(&response_cursor.into_inner(), from).await?;
    } else {
        if data.get(0..A2S_INFO_QUERY_STRING.len()) != Some(A2S_INFO_QUERY_STRING) {
            return Err(anyhow!("query string isn't correct"));
        }
        let challenge = data
            .get(A2S_INFO_QUERY_STRING.len()..A2S_INFO_QUERY_STRING.len() + 4)
            .ok_or_else(|| anyhow!("couldn't read challenge value"))?;
        let challenge =
            u32::from_le_bytes(challenge.try_into().expect("this should always be 4 bytes"));

        if challenge != get_challenge_for_address(from) {
            return Err(anyhow!("unexpected challenge"));
        };

        send_s2a_info_response(socket, from, configuration).await?;
    }

    Ok(())
}

async fn send_s2a_info_response(
    socket: &UdpSocket,
    from: SocketAddr,
    configuration: &Configuration,
) -> anyhow::Result<()> {
    let mut response_cursor = Cursor::new(Vec::<u8>::new());
    let mut response = BitWriter::endian(&mut response_cursor, LittleEndian);

    response.write_out::<32, _>(CONNECTIONLESS_HEADER)?;
    response.write_out::<8, _>(b'I')?; // S2A_INFO_SRC
    response.write_out::<8, _>(PROTOCOL_VERSION)?;
    write_string(&mut response, &configuration.server.server_name)?; // server name
    write_string(&mut response, "")?; // map name
    write_string(&mut response, "")?; // game dir
    write_string(&mut response, &configuration.server.app_desc)?; // game description
    response.write_out::<16, _>(configuration.server.app_id)?; // app id
    response.write_out::<8, _>(0)?; // player count
    response.write_out::<8, _>(0)?; // max players
    response.write_out::<8, _>(0)?; // bot count
    response.write_out::<8, _>(b'p')?; // server type, p is hltv relay/proxy (lol)
    response.write_out::<8, _>(b'l')?; // OS, hardcoded to linux for now
    response.write_out::<8, _>(0)?; // visibility, 0 for public
    response.write_out::<8, _>(0)?; // VAC, 0 for insecure (don't care since any server we redirect to can choose their own policy)
    write_string(&mut response, &configuration.server.app_version)?; // app version

    socket.send_to(&response_cursor.into_inner(), from).await?;

    Ok(())
}

/// Handle a connectionless packet. Returns a challenge number when the
/// connection handshake has completed, which should be provided to the
// netchannel
pub async fn process_connectionless_packet(
    socket: &UdpSocket,
    from: SocketAddr,
    data: &[u8],
    configuration: &Configuration,
) -> anyhow::Result<Option<u32>> {
    // Cut off connectionless header
    let data = data
        .get(4..)
        .ok_or_else(|| anyhow!("connectionless packet doesn't have any data"))?;

    let mut reader = BitReader::endian(Cursor::new(data), LittleEndian);

    let command: u8 = reader.read_in::<8, _>()?;

    match command {
        // A2S_GETCHALLENGE
        b'q' => handle_a2s_get_challenge(socket, from, &mut reader).await?,
        // C2S_CONNECT
        b'k' => return Ok(Some(handle_c2s_connect(socket, from, &mut reader).await?)),

        // A2S_INFO
        // slice access will never panic because the command is 1 byte
        b'T' => handle_a2s_info(socket, from, &data[1..], configuration).await?,
        // A2S_PLAYER, silently drop
        b'U' => {}
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
