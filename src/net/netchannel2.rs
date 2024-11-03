use std::{collections::VecDeque, io::Cursor, ops::Range};

use crate::io_util;

use super::message::Message;
use bitflags::bitflags;
use bitstream_io::{BitRead, BitReader, LittleEndian};
use strum::{EnumCount, EnumIter, IntoEnumIterator};
use thiserror::Error;
use tracing::trace;

#[derive(Error, Debug)]
pub enum NetChannelError {
    #[error("io error: {0}")]
    Io(std::io::Error),
    #[error("invalid checksum, expected {expected:04x}, got {actual:04x}")]
    InvalidChecksum { expected: u16, actual: u16 },
    #[error("mismatched challenge, expected {expected:08x}, got {actual:08x}")]
    MismatchedChallenge { expected: u32, actual: u32 },
    #[error("challenge expected, but not received")]
    MissingChallenge,
    #[error("out of order packet, highest seen sequence {current}, received {received}")]
    OutOfOrderPacket { current: i32, received: i32 },
    #[error("duplicate packet with sequence {0}")]
    DuplicatePacket(i32),
    #[error("no active transfer for stream {0:?}, but got incoming data anyways")]
    NoActiveTransfer(StreamType),
    #[error("transfer of {0} bytes is too large to create")]
    TransferTooLarge(u32),
    #[error("transfer data is out of bounds")]
    OutOfBoundsTransferData,
}

impl From<std::io::Error> for NetChannelError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

bitflags! {
    #[derive(Debug)]
    struct PacketFlags: u8 {
        /// packet contains reliable stream data
        const RELIABLE    = 1 << 0;
        /// packet is compressed (unused)
        const COMPRESSED  = 1 << 1;
        /// packet is encrypted (unused)
        const ENCRYPTED   = 1 << 2;
        /// packet is split (unused)
        const SPLIT       = 1 << 3;
        /// packet was choked by sender
        const CHOKED      = 1 << 4;
        /// packet contains a challenge number, use to prevent packet injection
        const CHALLENGE   = 1 << 5;
    }
}

#[repr(usize)]
#[derive(EnumIter, EnumCount, Clone, Copy, Debug)]
pub enum StreamType {
    Regular = 0,
    File = 1,
}

struct PerStream<T> {
    streams: [T; StreamType::COUNT],
}

impl<T> PerStream<T> {
    /// Create a new instance of PerStream. `f` is a function that takes a
    /// `Stream` and returns an instance of `T`
    fn new(f: impl Fn(StreamType) -> T) -> PerStream<T> {
        // This is stupid. I should use a Vec here...
        let mut streams: [_; StreamType::COUNT] = std::array::from_fn(|_| None);
        for stream in StreamType::iter() {
            streams[stream as usize] = Some(f(stream));
        }

        Self {
            streams: streams.map(Option::unwrap),
        }
    }

    /// Get a stream by its type.
    fn stream(&mut self, stream: StreamType) -> &mut T {
        &mut self.streams[stream as usize]
    }
}

const FRAGMENT_BITS: u32 = 8;
const FRAGMENT_SIZE: u32 = 1 << FRAGMENT_BITS;

const MAX_FILE_SIZE_BITS: u32 = 26;
const MAX_FILE_SIZE: u32 = (1 << MAX_FILE_SIZE_BITS) - 1;

const PATH_OSMAX: usize = 260;

struct OutgoingReliableTransfer {}

impl OutgoingReliableTransfer {
    /// Returns true if all the transfer data has been acknowledged
    fn completed(&self) -> bool {
        todo!()
    }
}

struct IncomingReliableTransfer {
    /// Data buffer to hold the incoming transfer
    buffer: Vec<u8>,
    /// The total size of the transfer. If the transfer is compressed, this is
    /// the compressed size.
    size: u32,
    /// The uncompressed size of the transfer. If the transfer isn't compressed,
    /// this is None.
    uncompressed_size: Option<u32>,
    /// Will the transfer be sent in multiple packets
    multi_block: bool,

    transfer_id: Option<u32>,
    /// Filename, used for file transfers only
    filename: Option<String>,

    /// Number of acknowledged fragments. When this matches the total number of
    /// fragments, the transfer is complete.
    acked_fragments: u32,
}

impl IncomingReliableTransfer {
    /// Read a transfer header and create a new incoming transfer
    fn new_from_header(
        reader: &mut impl BitRead,
        is_multi_block: bool,
    ) -> Result<Self, NetChannelError> {
        let bytes;
        let mut uncompressed_size = None;
        let mut transfer_id = None;
        let mut filename = None;

        if is_multi_block {
            // Is it a file?
            if reader.read_bit()? {
                transfer_id = Some(reader.read_in::<32, u32>()?);
                filename = Some(io_util::read_string(reader, PATH_OSMAX)?);
            }

            // Is it compressed?
            if reader.read_bit()? {
                uncompressed_size = Some(reader.read_in::<MAX_FILE_SIZE_BITS, u32>()?);
            }

            bytes = reader.read_in::<MAX_FILE_SIZE_BITS, u32>()?;
        } else {
            // Is it compressed?
            if reader.read_bit()? {
                uncompressed_size = Some(reader.read_in::<MAX_FILE_SIZE_BITS, u32>()?);
            }

            bytes = io_util::read_varint32(reader)?;
        }

        if bytes > MAX_FILE_SIZE {
            return Err(NetChannelError::TransferTooLarge(bytes));
        }

        let mut buffer = Vec::new();
        // NOTE: I'm not entirely sure if padding it is necessary, but Source
        // does this too.
        buffer.resize(
            usize::try_from(pad_number(bytes, 4))
                .map_err(|_| NetChannelError::TransferTooLarge(bytes))?,
            0,
        );

        Ok(Self {
            buffer,
            uncompressed_size,
            size: bytes,
            multi_block: is_multi_block,

            transfer_id,
            filename,

            acked_fragments: 0,
        })
    }

    fn num_fragments(&self) -> u32 {
        bytes_to_fragments(self.size)
    }

    /// Read data from the packet into the transfer. If the transfer is
    /// single-block, `start_fragment` and `num_fragments` are unused.
    fn read_data(
        &mut self,
        reader: &mut impl BitRead,
        start_fragment: u32,
        mut num_fragments: u32,
    ) -> Result<(), NetChannelError> {
        // Single block transfers
        if !self.multi_block {
            num_fragments = self.num_fragments();
        }

        let range = calculate_reliable_data_range(
            start_fragment,
            num_fragments,
            self.size,
            self.num_fragments(),
        )?;

        let data = self
            .buffer
            .get_mut(range)
            .ok_or(NetChannelError::OutOfBoundsTransferData)?;

        reader.read_bytes(data)?;
        self.acked_fragments += num_fragments;

        Ok(())
    }
}

fn calculate_reliable_data_range(
    start_fragment: u32,
    num_fragments: u32,
    total_bytes: u32,
    total_fragments: u32,
) -> Result<Range<usize>, NetChannelError> {
    let start_offset = start_fragment * FRAGMENT_SIZE;
    let mut length = num_fragments * FRAGMENT_SIZE;

    if start_fragment + num_fragments == total_fragments {
        // The length for the final fragment needs to be adjusted
        let rest = FRAGMENT_SIZE - (total_bytes % FRAGMENT_SIZE);
        if rest < FRAGMENT_SIZE {
            length -= rest;
        }
    }

    let start_offset =
        usize::try_from(start_offset).map_err(|_| NetChannelError::OutOfBoundsTransferData)?;
    let length = usize::try_from(length).map_err(|_| NetChannelError::OutOfBoundsTransferData)?;
    Ok(start_offset..start_offset + length)
}

/// Pad `number`` to be on a `boundary` byte boundary.  For example,
/// `pad_number(0, 4)` returns `0`, and `pad_number(1, 4)` returns 4.
fn pad_number(number: u32, boundary: u32) -> u32 {
    (number + (boundary - 1)) / boundary * boundary
}

fn bytes_to_fragments(bytes: u32) -> u32 {
    pad_number(bytes, FRAGMENT_SIZE) / FRAGMENT_SIZE
}

const MAX_SUBCHANNELS: usize = 8;

struct SubChannel {}

impl SubChannel {
    fn new() -> Self {
        Self {}
    }

    /// Update the subchannel. TODO: elaborate
    fn update(&mut self, acknowledged: bool) -> Result<(), NetChannelError> {
        todo!()
    }
}

/// Implementation of Source Engine NetChannels
pub struct NetChannel2 {
    /// Challenge number to validate packets with.
    challenge: u32,
    /// Has a challenge number been received before?
    has_seen_challenge: bool,

    /// Incoming sequence number
    in_sequence_nr: i32,
    /// Last acknowledged outgoing sequence number
    out_sequence_nr_ack: i32,

    /// Reliable state for outgoing subchannels
    out_reliable_state: u8,
    /// Outgoing reliable transfer data
    out_reliable_data: PerStream<VecDeque<OutgoingReliableTransfer>>,
    /// Subchannels for outgoing reliable data
    subchannels: [SubChannel; MAX_SUBCHANNELS],

    /// Reliable state for incoming subchannels
    in_reliable_state: u8,
    /// Incoming reliable transfer data
    in_reliable_data: PerStream<Option<IncomingReliableTransfer>>,
}

impl NetChannel2 {
    /// Create a new netchannel.
    pub fn new(challenge: u32) -> Self {
        Self {
            challenge,
            has_seen_challenge: false,

            in_sequence_nr: 0,
            out_sequence_nr_ack: 0,

            out_reliable_state: 0,
            out_reliable_data: PerStream::new(|_| VecDeque::new()),
            subchannels: std::array::from_fn(|_| SubChannel::new()),

            in_reliable_state: 0,
            in_reliable_data: PerStream::new(|_| None),
        }
    }

    /// Read a single packet and returns the messages that were received.
    pub fn read_packet(&mut self, packet: &[u8]) -> Result<Vec<Message>, NetChannelError> {
        let mut reader = BitReader::endian(Cursor::new(packet), LittleEndian);
        let mut messages = vec![];

        let flags = self.read_header(&mut reader, packet)?;
        if flags.contains(PacketFlags::RELIABLE) {
            self.read_reliable(&mut reader)?;
        }

        self.read_completed_incoming_transfers(&mut messages)?;
        self.read_messages(&mut reader, &mut messages)?;

        Ok(messages)
    }

    /// Read the packet header.
    fn read_header(
        &mut self,
        reader: &mut impl BitRead,
        packet: &[u8],
    ) -> Result<PacketFlags, NetChannelError> {
        let sequence = reader.read_in::<32, i32>()?;
        let sequence_ack = reader.read_in::<32, i32>()?;
        let flags: PacketFlags = PacketFlags::from_bits_truncate(reader.read_in::<8, u8>()?);

        // Offset of data after the checksum
        const CHECKSUM_DATA_OFFSET: usize = 11;
        let checksum = reader.read_in::<16, u16>()?;
        validate_checksum(checksum, &packet[CHECKSUM_DATA_OFFSET..])?;

        let reliable_state_ack = reader.read_in::<8, u8>()?;

        let mut num_choked: i32 = 0;
        if flags.contains(PacketFlags::CHOKED) {
            num_choked = reader.read_in::<8, i32>()?;
        }

        self.check_challenge(&flags, reader)?;
        self.check_sequence(sequence)?;

        let dropped_packets = sequence - (self.in_sequence_nr + num_choked + 1);
        if dropped_packets > 0 {
            trace!("dropped {dropped_packets} packets");
        }

        self.update_subchannel_acks(reliable_state_ack)?;

        self.in_sequence_nr = sequence;
        self.out_sequence_nr_ack = sequence_ack;

        self.pop_completed_outgoing_transfers();

        Ok(flags)
    }

    /// Check that the packet is not out-of-order or duplicated.
    fn check_sequence(&mut self, sequence: i32) -> Result<(), NetChannelError> {
        if sequence < self.in_sequence_nr {
            return Err(NetChannelError::OutOfOrderPacket {
                current: self.in_sequence_nr,
                received: sequence,
            });
        } else if sequence == self.in_sequence_nr {
            return Err(NetChannelError::DuplicatePacket(sequence));
        }

        Ok(())
    }

    /// Read the challenge number and check that it matches the expected
    /// challenge state.
    fn check_challenge(
        &mut self,
        flags: &PacketFlags,
        reader: &mut impl BitRead,
    ) -> Result<(), NetChannelError> {
        if flags.contains(PacketFlags::CHALLENGE) {
            let challenge = reader.read_in::<32, u32>()?;

            if challenge != self.challenge {
                return Err(NetChannelError::MismatchedChallenge {
                    expected: self.challenge,
                    actual: challenge,
                });
            }

            self.has_seen_challenge = true;
        } else if self.has_seen_challenge {
            return Err(NetChannelError::MissingChallenge);
        }

        Ok(())
    }

    /// Update the subchannels based on the acknowledged reliable state.
    fn update_subchannel_acks(&mut self, reliable_state_ack: u8) -> Result<(), NetChannelError> {
        for subchannel_index in 0..MAX_SUBCHANNELS {
            let mask = 1 << subchannel_index;
            let subchannel = &mut self.subchannels[subchannel_index];
            let acknowledged = (self.out_reliable_state) == (reliable_state_ack & mask);

            subchannel.update(acknowledged)?;
        }

        todo!()
    }

    /// Remove completed outgoing transfers from the queue
    fn pop_completed_outgoing_transfers(&mut self) {
        for stream in StreamType::iter() {
            let transfers = self.out_reliable_data.stream(stream);
            let Some(transfer) = transfers.front() else {
                continue;
            };

            if transfer.completed() {
                transfers.pop_front();
            }
        }
    }

    fn read_reliable(&mut self, reader: &mut impl BitRead) -> Result<(), NetChannelError> {
        let subchannel_index = reader.read_in::<3, u8>()?;

        for stream in StreamType::iter() {
            if reader.read_bit()? {
                self.read_subchannel_data(reader, stream)?;
            }
        }

        // Flip subchannel bit to signal acknowledgement
        self.in_reliable_state ^= 1 << subchannel_index;

        Ok(())
    }

    fn read_subchannel_data(
        &mut self,
        reader: &mut impl BitRead,
        stream: StreamType,
    ) -> Result<(), NetChannelError> {
        let mut start_fragment = 0;
        let mut num_fragments = 0;

        let is_multi_block = reader.read_bit()?;
        if is_multi_block {
            start_fragment = reader.read_in::<{ MAX_FILE_SIZE_BITS - FRAGMENT_BITS }, u32>()?;
            num_fragments = reader.read_in::<3, u32>()?;
        }

        // Start of the transfer, read the header
        if start_fragment == 0 {
            *self.in_reliable_data.stream(stream) = Some(
                IncomingReliableTransfer::new_from_header(reader, is_multi_block)?,
            );
        }

        let transfer = self
            .in_reliable_data
            .stream(stream)
            .as_mut()
            .ok_or(NetChannelError::NoActiveTransfer(stream))?;

        transfer.read_data(reader, start_fragment, num_fragments)?;

        todo!()
    }

    /// Read any completed reliable message transfers.
    fn read_completed_incoming_transfers(
        &self,
        messages: &[Message],
    ) -> Result<(), NetChannelError> {
        todo!()
    }

    /// Read all remaining messages from a bitstream. Any messages that are read
    /// will be appended to `messages`.
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
    pub fn queue_reliable_messages(&mut self, messages: &[Message]) -> Result<(), NetChannelError> {
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

    #[test]
    fn test_validate_checksum_matching() {
        // calculated using https://www.crccalc.com/?crc=01020304&method=CRC-32%2FISO-HDLC&datatype=1&outtype=0
        const EXPECTED_CHECKSUM: u16 = 0xb63c ^ 0xfbcd;

        assert!(validate_checksum(EXPECTED_CHECKSUM, &[1, 2, 3, 4]).is_ok());
    }

    #[test]
    fn test_validate_checksum_non_matching() {
        const EXPECTED_CHECKSUM: u16 = 0xb63c ^ 0xfbcd;
        const ACTUAL_CHECKSUM: u16 = 0xe951 ^ 0xa406;

        assert!(
            validate_checksum(EXPECTED_CHECKSUM, &[4, 3, 2, 1]).is_err_and(|e| {
                match e {
                    NetChannelError::InvalidChecksum { expected, actual } => {
                        expected == EXPECTED_CHECKSUM && actual == ACTUAL_CHECKSUM
                    }

                    _ => false,
                }
            })
        );
    }

    #[test]
    fn test_pad_number() {
        assert_eq!(pad_number(0, 4), 0, "0 should be rounded to 0");
        assert_eq!(pad_number(1, 4), 4, "1 should be rounded up to 4");
        assert_eq!(pad_number(4, 4), 4, "4 should remain as 4");
        assert_eq!(pad_number(5, 4), 8, "5 should be rounded up to 8");
    }

    #[test]
    fn test_bytes_to_fragments() {
        assert_eq!(bytes_to_fragments(0), 0, "0 bytes takes 0 fragments");
        assert_eq!(bytes_to_fragments(1), 1, "1 byte takes 1 fragment");
        assert_eq!(
            bytes_to_fragments(FRAGMENT_SIZE - 1),
            1,
            "FRAGMENT_SIZE - 1 should take 1 fragment"
        );
        assert_eq!(
            bytes_to_fragments(FRAGMENT_SIZE),
            1,
            "FRAGMENT_SIZE should take 1 fragment"
        );
        assert_eq!(
            bytes_to_fragments(FRAGMENT_SIZE + 1),
            2,
            "FRAGMENT_SIZE + 1 should take 2 fragments"
        );
        assert_eq!(
            bytes_to_fragments(FRAGMENT_SIZE * 10),
            10,
            "FRAGMENT_SIZE * 1 should take 10 fragments"
        );
    }

    #[test]
    fn test_calculate_reliable_data_range() {
        // I took some shortcuts here,but this won't cause problems unless
        // you're running on a 16-bit architecture. (at that point, good luck
        // :D). This doesn't affect calculate_reliable_data_range itself, as it
        // does the conversion correctly.
        assert!(
            usize::try_from(FRAGMENT_SIZE).is_ok(),
            "FRAGMENT_SIZE must fit into a usize for these tests for be correct"
        );

        assert_eq!(
            calculate_reliable_data_range(0, 1, 4, 1).unwrap(),
            0..4,
            "chunks for transfers smaller than a fragment should not take an entire fragment"
        );
        assert_eq!(
            calculate_reliable_data_range(1, 1, FRAGMENT_SIZE + 4, 2).unwrap(),
            FRAGMENT_SIZE as usize..FRAGMENT_SIZE as usize + 4,
            "tailing chunks should not take an entire fragment"
        );
        assert_eq!(
            calculate_reliable_data_range(0, 1, FRAGMENT_SIZE + 4, 2).unwrap(),
            0..FRAGMENT_SIZE as usize,
            "beginning chunks should take an entire fragment"
        );
    }
}
