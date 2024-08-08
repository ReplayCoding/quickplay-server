mod connectionless;

use std::{borrow::Cow, net::UdpSocket};

use anyhow::anyhow;
use log::{debug, info, trace};

const CONNECTIONLESS_HEADER: [u8; 4] = (-1_i32).to_le_bytes();
const SPLITPACKET_HEADER: [u8; 4] = (-2_i32).to_le_bytes();
const COMPRESSEDPACKET_HEADER: [u8; 4] = (-3_i32).to_le_bytes();

fn decode_raw_packet(packet_buf: &[u8]) -> anyhow::Result<Cow<'_, [u8]>> {
    let header_flags = packet_buf
        .get(0..4)
        .ok_or_else(|| anyhow!("couldn't get header flags"))?;

    if header_flags == SPLITPACKET_HEADER {
        todo!("handle splitpacket");
    }

    if header_flags == COMPRESSEDPACKET_HEADER {
        todo!("handle compressed packet");
    }

    Ok(Cow::Borrowed(packet_buf))
}

fn process_packet(packet_buf: &[u8]) -> anyhow::Result<()> {
    let packet_buf = decode_raw_packet(packet_buf)?;

    let header_flags = packet_buf
        .get(0..4)
        .ok_or_else(|| anyhow!("couldn't get header flags"))?;

    if header_flags == CONNECTIONLESS_HEADER {
        connectionless::process_connectionless_packet(&packet_buf);
    } else {
        todo!("netchannels");
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let socket = UdpSocket::bind("127.0.0.2:4444")?;
    info!("bound to address {:?}", socket.local_addr()?);

    // This is *seriously* overkill, but we can fix it later;
    let mut packet_buf = vec![0u8; u32::MAX.try_into().expect("cannot allocate packet buf")];
    loop {
        let (packet_size, addr) = socket.recv_from(&mut packet_buf)?;
        let packet_buf = &packet_buf[..packet_size];

        trace!("got packet of size {} from {:?}", packet_size, addr);

        if let Err(err) = process_packet(packet_buf) {
            debug!("malformed packet from {:?}: {:?}", addr, err);
        };
    }
}
