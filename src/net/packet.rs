use std::borrow::Cow;
use thiserror::Error;

use super::compression::{self, CompressionError};

pub const CONNECTIONLESS_HEADER: u32 = -1_i32 as u32;
const SPLITPACKET_HEADER: u32 = -2_i32 as u32;
const COMPRESSEDPACKET_HEADER: u32 = -3_i32 as u32;

#[derive(Error, Debug)]
pub enum PacketDecoderError {
    #[error("no header flags found")]
    NoHeaderFlags,
    #[error("split packets are currently unimplemented")]
    SplitPacket,
    #[error("compression error: {0:?}")]
    Compression(CompressionError),
}

/// A decompressed/reassembled packet.
pub enum Packet<'a> {
    Connectionless(Cow<'a, [u8]>),
    Regular(Cow<'a, [u8]>),
}

/// Take a raw packet and returns the decompressed/reassembled data
pub fn decode_raw_packet(packet_data: &[u8]) -> Result<Packet, PacketDecoderError> {
    let header_flags = packet_data
        .get(0..4)
        .ok_or(PacketDecoderError::NoHeaderFlags)?;

    if header_flags == SPLITPACKET_HEADER.to_le_bytes() {
        return Err(PacketDecoderError::SplitPacket);
    }

    let packet_data = if header_flags == COMPRESSEDPACKET_HEADER.to_le_bytes() {
        // Slice is safe because header_flags is 4 bytes long
        let decompressed_packet =
            compression::decompress(&packet_data[4..]).map_err(PacketDecoderError::Compression)?;

        Cow::Owned(decompressed_packet)
    } else {
        Cow::Borrowed(packet_data)
    };

    let inner_header_flags = packet_data
        .get(0..4)
        .ok_or(PacketDecoderError::NoHeaderFlags)?;

    Ok(
        if inner_header_flags == CONNECTIONLESS_HEADER.to_le_bytes() {
            Packet::Connectionless(packet_data)
        } else {
            Packet::Regular(packet_data)
        },
    )
}
