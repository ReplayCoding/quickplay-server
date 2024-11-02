use std::io::Cursor;

use super::message::Message;
use bitflags::bitflags;
use bitstream_io::{BitRead, BitReader, LittleEndian};
use thiserror::Error;

bitflags! {
    #[derive(Debug)]
    struct PacketFlags: u8 {
        /// packet contains reliable stream data
        const PACKET_FLAG_RELIABLE    = 1 << 0;
        /// packet is compressed (unused)
        const PACKET_FLAG_COMPRESSED  = 1 << 1;
        /// packet is encrypted (unused)
        const PACKET_FLAG_ENCRYPTED   = 1 << 2;
        /// packet is split (unused)
        const PACKET_FLAG_SPLIT       = 1 << 3;
        /// packet was choked by sender
        const PACKET_FLAG_CHOKED      = 1 << 4;
        /// packet contains a challenge number, use to prevent packet injection
        const PACKET_FLAG_CHALLENGE   = 1 << 5;
    }
}

#[derive(Error, Debug)]
enum NetChannelError {
    #[error("io error: {0}")]
    Io(std::io::Error),
    #[error("invalid checksum, expected {expected:04x}, got {actual:04x}")]
    InvalidChecksum { expected: u16, actual: u16 },
}

impl From<std::io::Error> for NetChannelError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Implementation of Source Engine NetChannels
struct NetChannel2 {}

impl NetChannel2 {
    /// Create a new netchannel.
    fn new() -> Self {
        Self {}
    }

    /// Read a single packet and returns the messages that were read.
    pub fn read_packet(&mut self, packet: &[u8]) -> Result<Vec<Message>, NetChannelError> {
        let mut reader = BitReader::endian(Cursor::new(packet), LittleEndian);
        let mut messages = vec![];

        let flags = self.read_header(&mut reader, packet)?;
        if flags.contains(PacketFlags::PACKET_FLAG_RELIABLE) {
            self.read_reliable(&mut reader)?;
        }

        self.read_messages(&mut reader, &mut messages)?;

        Ok(messages)
    }

    /// Read the packet header.
    fn read_header(
        &self,
        reader: &mut impl BitRead,
        packet: &[u8],
    ) -> Result<PacketFlags, NetChannelError> {
        let sequence = reader.read_in::<32, i32>()?;
        let sequence_ack = reader.read_in::<32, i32>()?;
        let flags: PacketFlags = PacketFlags::from_bits_truncate(reader.read_in::<8, u8>()?);

        /// Offset of data after the checksum
        const CHECKSUM_DATA_OFFSET: usize = 11;
        let checksum = reader.read_in::<16, u16>()?;
        validate_checksum(checksum, &packet[CHECKSUM_DATA_OFFSET..])?;

        Ok(flags)
    }

    fn read_reliable(&self, reader: &mut impl BitRead) -> Result<(), NetChannelError> {
        todo!()
    }

    /// Read messages from a bitstream. Any messages that are read will be appended to `messages`.
    fn read_messages(
        &self,
        reader: &mut impl BitRead,
        messages: &mut Vec<Message>,
    ) -> Result<(), NetChannelError> {
        todo!()
    }

    /// Write a single packet. Any messages in `messages` will be sent as unreliable.
    pub fn write_packet(&mut self, messages: &[Message]) -> Result<Vec<u8>, NetChannelError> {
        todo!()
    }

    /// Queue reliable messages to be sent.
    pub fn queue_reliable_messages(messages: &[Message]) -> Result<(), NetChannelError> {
        todo!()
    }
}

/// Validate that `checksum` matches the checksum for `data`. Returns Ok(()) if
/// they match.
#[allow(unused)]
fn validate_checksum(checksum: u16, data: &[u8]) -> Result<(), NetChannelError> {
    let calculated_checksum = calculate_checksum(data);

    if calculated_checksum == checksum {
        Ok(())
    } else {
        Err(NetChannelError::InvalidChecksum {
            expected: checksum,
            actual: calculated_checksum,
        })
    }
}

/// Calculate the checksum for `data`.
fn calculate_checksum(data: &[u8]) -> u16 {
    let hasher = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
    let crc = hasher.checksum(data);

    let low_part = crc & 0xffff;
    let high_part = (crc >> 16) & 0xffff;

    (low_part ^ high_part) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_empty() {
        assert_eq!(calculate_checksum(&[]), 0);
    }

    #[test]
    fn test_checksum_with_data() {
        // calculated using https://www.crccalc.com/?crc=01020304&method=CRC-32%2FISO-HDLC&datatype=1&outtype=0
        const EXPECTED_CHECKSUM: u16 = 0xb63c ^ 0xfbcd;

        assert_eq!(calculate_checksum(&[1, 2, 3, 4]), EXPECTED_CHECKSUM);
    }
}
