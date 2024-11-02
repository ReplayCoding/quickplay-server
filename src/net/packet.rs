use std::borrow::Cow;
use thiserror::Error;

pub const CONNECTIONLESS_HEADER: u32 = -1_i32 as u32;
const SPLITPACKET_HEADER: u32 = -2_i32 as u32;
const COMPRESSEDPACKET_HEADER: u32 = -3_i32 as u32;

const COMPRESSION_SNAPPY: &[u8] = b"SNAP";

#[derive(Error, Debug)]
pub enum PacketDecoderError {
    #[error("no header flags found")]
    NoHeaderFlags,
    #[error("split packets are currently unimplemented")]
    SplitPacket,
    #[error("no compression type found")]
    NoCompressionType,
    #[error("unknown compression type: {0:02x?}")]
    UnhandledCompressionType([u8; 4]),
    #[error("no compressed data found")]
    NoCompressedData,
    #[error("decompression error: {0:?}")]
    DecompressionError(snap::Error),
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
        let decompressed_packet = decompress_packet(packet_data)?;

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

fn decompress_packet(packet_data: &[u8]) -> Result<Vec<u8>, PacketDecoderError> {
    let compression_type = packet_data
        .get(4..8)
        .ok_or(PacketDecoderError::NoCompressionType)?;

    match compression_type {
        COMPRESSION_SNAPPY => {
            let compressed_data = packet_data
                .get(8..)
                .ok_or(PacketDecoderError::NoCompressedData)?;

            let mut decoder = snap::raw::Decoder::new();
            decoder
                .decompress_vec(compressed_data)
                .map_err(PacketDecoderError::DecompressionError)
        }

        _ => Err(PacketDecoderError::UnhandledCompressionType(
            // .unwrap() will never fail because compression_type is always
            // exactly 4 bytes
            compression_type.try_into().unwrap(),
        )),
    }
}
