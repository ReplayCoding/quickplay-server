use std::{collections::VecDeque, io::Cursor, ops::Range};

use crate::io_util;

use super::{
    compression::{self, CompressionError},
    message::{read_messages, write_messages, NetMessage, MessageSide},
};
use bitflags::bitflags;
use bitstream_io::{BitRead, BitReader, BitWrite, BitWriter, LittleEndian};
use strum::{EnumCount, EnumDiscriminants, EnumIter, IntoEnumIterator};
use thiserror::Error;
use tracing::trace;

#[derive(Error, Debug)]
pub enum NetChannelError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
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
    #[error("transfer of is too large to create")]
    TransferTooLarge,
    #[error("transfer data is out of bounds")]
    OutOfBoundsTransferData,
    #[error("tried to access data when transfer is incomplete")]
    IncompleteTransfer,
    #[error("compression error: {0:?}")]
    Compression(CompressionError),
    #[error("invalid reliable state")]
    InvalidReliableState,
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
    Message = 0,
    File = 1,
}

#[derive(Debug, Clone)]
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

    /// Get a mutable stream by its type.
    fn stream_mut(&mut self, stream: StreamType) -> &mut T {
        &mut self.streams[stream as usize]
    }

    /// Get a stream by its type.
    fn stream(&self, stream: StreamType) -> &T {
        &self.streams[stream as usize]
    }
}

/// A range that stores length, instead of the end of the range.
#[derive(PartialEq, Debug, Clone, Copy)]
struct LengthRange<T> {
    start: T,
    length: T,
}

impl<T: Copy + std::ops::Add<Output = T>> LengthRange<T> {
    fn new(start: T, length: T) -> Self {
        Self { start, length }
    }

    fn try_cast<C>(&self) -> Result<LengthRange<C>, <C as TryFrom<T>>::Error>
    where
        C: TryFrom<T> + Copy + std::ops::Add<Output = C>,
    {
        Ok(LengthRange::new(
            C::try_from(self.start)?,
            C::try_from(self.length)?,
        ))
    }

    fn into_range(self) -> Range<T> {
        self.start..(self.start + self.length)
    }
}

const FRAGMENT_BITS: u32 = 8;
const FRAGMENT_SIZE: u32 = 1 << FRAGMENT_BITS;

const MAX_FILE_SIZE_BITS: u32 = 26;
const MAX_FILE_SIZE: u32 = (1 << MAX_FILE_SIZE_BITS) - 1;

const PATH_OSMAX: usize = 260;

#[derive(Debug, PartialEq, Eq)]
pub enum TransferType {
    Message,
    File { transfer_id: u32, filename: String },
}

struct OutgoingReliableTransfer {
    /// The data to be sent
    data: Vec<u8>,
    /// If the data is compressed, then this contains the size of the data when
    /// uncompressed.
    uncompressed_size: Option<u32>,
    /// The type of the transfer
    transfer_type: TransferType,

    /// Number of sent fragments
    sent_fragments: u32,
    /// Number of acknowledged fragments
    acked_fragments: u32,
}

impl OutgoingReliableTransfer {
    fn new(transfer_type: TransferType, data: Vec<u8>) -> Result<Self, NetChannelError> {
        let mut data = data;

        let size: u32 = data
            .len()
            .try_into()
            .map_err(|_| NetChannelError::TransferTooLarge)?;
        if size > MAX_FILE_SIZE {
            return Err(NetChannelError::TransferTooLarge);
        }

        let mut uncompressed_size = None;

        let compressed_data = compression::compress(&data).map_err(NetChannelError::Compression)?;
        // Only send compressed if it's actually smaller.
        if compressed_data.len() < data.len() {
            trace!(
                "compressed transfer: uncompressed {} compressed {}",
                data.len(),
                compressed_data.len()
            );
            data = compressed_data;
            uncompressed_size = Some(size);
        }

        Ok(Self {
            data,
            uncompressed_size,
            transfer_type,
            sent_fragments: 0,
            acked_fragments: 0,
        })
    }

    /// Return the total number of bytes that will be sent in the transfer.
    fn size(&self) -> u32 {
        // This cast is alright because the size is checked in `new`
        self.data.len() as u32
    }

    /// Return the total number of fragments that will be sent in the transfer.
    fn total_fragments(&self) -> u32 {
        bytes_to_fragments(self.size())
    }

    /// Return the number of fragments that have been sent or have been reserved
    /// by a subchannel.
    fn sent_fragments(&self) -> u32 {
        self.sent_fragments
    }

    fn acked_fragments(&self) -> u32 {
        self.acked_fragments
    }

    fn data(&self) -> &[u8] {
        &self.data
    }

    fn transfer_type(&self) -> &TransferType {
        &self.transfer_type
    }

    fn uncompressed_size(&self) -> Option<u32> {
        self.uncompressed_size
    }

    fn mark_sent(&mut self, num_fragments: u32) {
        self.sent_fragments += num_fragments;
    }

    fn mark_acked(&mut self, num_fragments: u32) {
        self.acked_fragments += num_fragments;
    }
}

const SUBCHANNEL_FRAGMENT_COUNT_BITS: u32 = 3;
// TODO: What is a reasonable size for this?
const SUBCHANNEL_MAX_SEND_SIZE: u32 = u32::MAX;
const MAX_SUBCHANNELS: usize = 8;

#[derive(EnumDiscriminants)]
#[strum_discriminants(name(SubchannelStateType))]
enum SubchannelState {
    Free,
    WaitingToSend {
        fragments: PerStream<Option<LengthRange<u32>>>,
    },
    WaitingForAck {
        fragments: PerStream<Option<LengthRange<u32>>>,
        send_seq_nr: i32,
    },
}

type OutgoingReliableTransfers = PerStream<VecDeque<OutgoingReliableTransfer>>;

struct Subchannel {
    state: SubchannelState,
    /// This is flipped every time the channel is filled. If it doesn't match up
    /// with the acknowledged reliable state, then we know the subchannel data
    /// hasn't been received.
    reliable_state: bool,
}

impl Subchannel {
    /// Create a new free subchannel.
    fn new() -> Self {
        Self {
            state: SubchannelState::Free,
            reliable_state: false,
        }
    }

    /// Returns the current state of the subchannel.
    fn state(&self) -> SubchannelStateType {
        SubchannelStateType::from(&self.state)
    }

    /// Try to fill the subchannel with the frontmost transfer. Will panic if
    /// the subchannel is not in the free state.
    fn fill(
        &mut self,
        transfers: &mut OutgoingReliableTransfers,
        mut budget: u32,
    ) -> Result<(), NetChannelError> {
        assert!(self.state() == SubchannelStateType::Free);

        let mut filled = false;
        let mut fragment_streams = PerStream::new(|_| None);
        for stream in StreamType::iter() {
            let Some(transfer) = transfers.stream_mut(stream).front_mut() else {
                // No transfers to send
                continue;
            };

            if transfer.total_fragments() == transfer.sent_fragments() {
                // No fragments left to fill
                continue;
            }

            let start_fragment = transfer.sent_fragments();
            let num_fragments = (transfer.total_fragments() - transfer.sent_fragments())
                .min(budget) // don't send more fragments than we have budget for
                .min((1 << SUBCHANNEL_FRAGMENT_COUNT_BITS) - 1); // don't try to fill more fragments than can sucessfully written

            trace!(
                "filled subchannel with fragments. start {start_fragment}, length {num_fragments}"
            );
            *fragment_streams.stream_mut(stream) =
                Some(LengthRange::new(start_fragment, num_fragments));
            transfer.mark_sent(num_fragments);
            filled = true;

            budget -= num_fragments;
            if budget == 0 {
                // Can't write anything more
                break;
            }
        }

        if filled {
            self.state = SubchannelState::WaitingToSend {
                fragments: fragment_streams,
            };
            // If any data is filled, reliable state must be flipped
            self.reliable_state = !self.reliable_state;
        }

        Ok(())
    }

    /// Write the subchannel to a bitstream. Will panic if the subchannel isn't
    /// waiting to send.
    fn write(
        &mut self,
        writer: &mut impl BitWrite,
        transfers: &OutgoingReliableTransfers,
        subchannel_index: u8,
        send_seq_nr: i32,
    ) -> Result<(), NetChannelError> {
        let SubchannelState::WaitingToSend { fragments } = &self.state else {
            panic!("Subchannel isn't waiting to send!")
        };

        writer.write_out::<3, _>(subchannel_index)?;

        for stream in StreamType::iter() {
            let Some(fragments) = fragments.stream(stream) else {
                // No data for this stream
                writer.write_bit(false)?;
                continue;
            };
            let transfer = transfers
                .stream(stream)
                .front()
                .expect("Writing subchannel data, but not active transfer!");

            // Signal that we have data for this stream
            writer.write_bit(true)?;

            let bytes = calculate_reliable_data_range(
                fragments.start,
                fragments.length,
                transfer.size(),
                transfer.total_fragments(),
            );
            trace!("writing stream {stream:?} in subchannel {subchannel_index}. fragments {fragments:?}, bytes {bytes:?}");

            self.write_header(writer, transfer, *fragments, bytes)?;

            let bytes = bytes
                .try_cast::<usize>()
                .map_err(|_| NetChannelError::TransferTooLarge)?
                .into_range();
            writer.write_bytes(&transfer.data()[bytes])?;
        }

        self.state = SubchannelState::WaitingForAck {
            fragments: fragments.clone(),
            send_seq_nr,
        };

        Ok(())
    }

    fn write_header(
        &self,
        writer: &mut impl BitWrite,
        transfer: &OutgoingReliableTransfer,
        fragments: LengthRange<u32>,
        bytes: LengthRange<u32>,
    ) -> Result<(), NetChannelError> {
        let is_single_block = (fragments.length == transfer.total_fragments())
            && transfer.transfer_type() == &TransferType::Message;

        writer.write_bit(!is_single_block)?;
        if is_single_block {
            assert!(
                fragments.start == 0,
                "sending all fragments, but not starting at 0?"
            );
            assert!(
                bytes.length == transfer.size(),
                "sending all fragments, but byte length doesn't match?"
            );

            if let Some(uncompressed_size) = transfer.uncompressed_size() {
                writer.write_bit(true)?;
                writer.write_out::<MAX_FILE_SIZE_BITS, _>(uncompressed_size)?;
            } else {
                writer.write_bit(false)?;
            }

            io_util::write_varint32(writer, transfer.size())?;
        } else {
            writer.write_out::<{ MAX_FILE_SIZE_BITS - FRAGMENT_BITS }, _>(fragments.start)?;
            writer.write_out::<SUBCHANNEL_FRAGMENT_COUNT_BITS, _>(fragments.length)?;

            // If we're sending the first fragment, write the header.
            if fragments.start == 0 {
                match transfer.transfer_type() {
                    TransferType::Message => writer.write_bit(false)?,
                    TransferType::File {
                        transfer_id,
                        filename,
                    } => {
                        writer.write_bit(true)?;
                        writer.write_out::<32, _>(*transfer_id)?;
                        io_util::write_string(writer, filename)?;
                    }
                }

                if let Some(uncompressed_size) = transfer.uncompressed_size() {
                    writer.write_bit(true)?;
                    writer.write_out::<MAX_FILE_SIZE_BITS, _>(uncompressed_size)?;
                } else {
                    writer.write_bit(false)?;
                }

                writer.write_out::<MAX_FILE_SIZE_BITS, _>(transfer.size())?;
            }
        }

        Ok(())
    }

    /// Update the subchannel state, based on the acknowledgement state.
    fn update(
        &mut self,
        transfers: &mut OutgoingReliableTransfers,
        acknowledged_state: bool,
        sequence_ack: i32,
    ) -> Result<(), NetChannelError> {
        if acknowledged_state == self.reliable_state {
            match &self.state {
                SubchannelState::Free | SubchannelState::WaitingToSend { fragments: _ } => {}
                SubchannelState::WaitingForAck {
                    fragments,
                    send_seq_nr,
                } => {
                    if *send_seq_nr > sequence_ack {
                        return Err(NetChannelError::InvalidReliableState);
                    }

                    self.mark_acked(transfers, fragments);
                    self.state = SubchannelState::Free;
                }
            }
        } else {
            match &self.state {
                SubchannelState::Free | SubchannelState::WaitingToSend { fragments: _ } => {}
                SubchannelState::WaitingForAck {
                    fragments,
                    send_seq_nr,
                } => {
                    if *send_seq_nr <= sequence_ack {
                        trace!("resending subchannel. fragments {fragments:?}");
                        self.state = SubchannelState::WaitingToSend {
                            fragments: fragments.clone(),
                        };
                    }
                }
            }
        }

        Ok(())
    }

    fn mark_acked(
        &self,
        transfers: &mut OutgoingReliableTransfers,
        fragments: &PerStream<Option<LengthRange<u32>>>,
    ) {
        for stream in StreamType::iter() {
            let Some(fragments) = fragments.stream(stream) else {
                continue;
            };
            let transfer = transfers
                .stream_mut(stream)
                .front_mut()
                .expect("acknowledging fragments, but no transfer!");

            transfer.mark_acked(fragments.length);
        }
    }
}

pub struct ReceivedFile {
    pub transfer_id: u32,
    pub filename: String,
    pub data: Vec<u8>,
}

#[derive(Debug)]
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

    /// The type of the transfer.
    transfer_type: TransferType,

    /// Number of acknowledged fragments. When this matches the total number of
    /// fragments, the transfer is complete.
    received_fragments: u32,
}

impl IncomingReliableTransfer {
    /// Read a transfer header and create a new incoming transfer
    fn new_from_header(
        reader: &mut impl BitRead,
        is_multi_block: bool,
    ) -> Result<Self, NetChannelError> {
        let bytes;
        let mut uncompressed_size = None;

        let transfer_type;
        if is_multi_block {
            // Is it a file?
            if reader.read_bit()? {
                let transfer_id = reader.read_in::<32, u32>()?;
                let filename = io_util::read_string(reader, PATH_OSMAX)?;

                transfer_type = TransferType::File {
                    transfer_id,
                    filename,
                };
            } else {
                transfer_type = TransferType::Message;
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

            // Single block transfer can't be file transfers.
            transfer_type = TransferType::Message;
            bytes = io_util::read_varint32(reader)?;
        }

        if bytes > MAX_FILE_SIZE {
            return Err(NetChannelError::TransferTooLarge);
        }

        let buffer =
            vec![0u8; usize::try_from(bytes).map_err(|_| NetChannelError::TransferTooLarge)?];

        trace!(
            "creating incoming transfer, is multi block: {is_multi_block}, \
                total bytes: {bytes}, uncompressed size: {uncompressed_size:?}, \
                type: {transfer_type:?}"
        );

        Ok(Self {
            buffer,
            uncompressed_size,
            size: bytes,
            multi_block: is_multi_block,

            transfer_type,

            received_fragments: 0,
        })
    }

    /// Read data from the packet into the transfer. If the transfer is
    /// single-block, `start_fragment` and `num_fragments` are unused.
    fn read_data(
        &mut self,
        reader: &mut impl BitRead,
        mut start_fragment: u32,
        mut num_fragments: u32,
    ) -> Result<(), NetChannelError> {
        // Single block transfers
        if !self.multi_block {
            start_fragment = 0;
            num_fragments = self.num_fragments();
        }

        let range = calculate_reliable_data_range(
            start_fragment,
            num_fragments,
            self.size,
            self.num_fragments(),
        );

        trace!(
            "reading {num_fragments} fragments starting at {start_fragment}. byte range is {:?}",
            range
        );
        let range = range
            .try_cast::<usize>()
            .map_err(|_| NetChannelError::OutOfBoundsTransferData)?
            .into_range();

        let data = self
            .buffer
            .get_mut(range)
            .ok_or(NetChannelError::OutOfBoundsTransferData)?;

        reader.read_bytes(data)?;
        self.received_fragments += num_fragments;

        Ok(())
    }

    /// Returns the total number of fragments in the transfer.
    fn num_fragments(&self) -> u32 {
        bytes_to_fragments(self.size)
    }

    /// Returns true if the all fragments have been received
    fn completed(&self) -> bool {
        self.received_fragments == self.num_fragments()
    }

    /// Return the data received in the transfer.
    fn data(self) -> Result<(TransferType, Vec<u8>), NetChannelError> {
        if !self.completed() {
            return Err(NetChannelError::IncompleteTransfer);
        }

        Ok((
            self.transfer_type,
            match self.uncompressed_size {
                Some(_uncompressed_size) => {
                    compression::decompress(&self.buffer).map_err(NetChannelError::Compression)?
                }
                // No compression, just use the buffer as-is
                None => self.buffer,
            },
        ))
    }
}

fn calculate_reliable_data_range(
    start_fragment: u32,
    num_fragments: u32,
    total_bytes: u32,
    total_fragments: u32,
) -> LengthRange<u32> {
    let start_offset = start_fragment * FRAGMENT_SIZE;
    let mut length = num_fragments * FRAGMENT_SIZE;

    if start_fragment + num_fragments == total_fragments {
        // The length for the final fragment needs to be adjusted
        let rest = FRAGMENT_SIZE - (total_bytes % FRAGMENT_SIZE);
        if rest < FRAGMENT_SIZE {
            length -= rest;
        }
    }

    LengthRange::new(start_offset, length)
}

/// Pad `number` to be on a `boundary` byte boundary.  For example,
/// `pad_number(0, 4)` returns `0`, and `pad_number(1, 4)` returns 4.
fn pad_number(number: u32, boundary: u32) -> u32 {
    (number + (boundary - 1)) / boundary * boundary
}

fn bytes_to_fragments(bytes: u32) -> u32 {
    pad_number(bytes, FRAGMENT_SIZE) / FRAGMENT_SIZE
}

/// Implementation of Source Engine NetChannels
pub struct NetChannel {
    /// The side of the connection that this netchannel is on
    side: MessageSide,

    /// Challenge number to validate packets with.
    challenge: u32,
    /// Has a challenge number been received before?
    has_seen_challenge: bool,

    /// Outgoing sequence number
    out_sequence_nr: i32,

    /// Incoming sequence number
    in_sequence_nr: i32,
    /// Last acknowledged outgoing sequence number
    out_sequence_nr_ack: i32,

    /// Reliable state for incoming subchannels
    in_reliable_state: u8,
    /// Incoming reliable transfer data
    in_reliable_transfers: PerStream<Option<IncomingReliableTransfer>>,

    /// Outgoing reliable transfer data
    out_reliable_transfers: OutgoingReliableTransfers,
    /// Outgoing subchannels
    subchannels: [Subchannel; MAX_SUBCHANNELS],

    /// Number of choked packets. TODO: what is this used for?
    num_choked: i32,
}

/// Offset of the flags in a packet.
const FLAGS_OFFSET: usize = 8;
/// Offset of the checksum in a packet.
const CHECKSUM_OFFSET: usize = 9;

impl NetChannel {
    /// Create a new netchannel.
    pub fn new(side: MessageSide, challenge: u32) -> Self {
        Self {
            side,

            challenge,
            has_seen_challenge: false,

            out_sequence_nr: 1,

            in_sequence_nr: 0,
            out_sequence_nr_ack: 0,

            in_reliable_state: 0,
            in_reliable_transfers: PerStream::new(|_| None),

            out_reliable_transfers: PerStream::new(|_| VecDeque::new()),
            subchannels: std::array::from_fn(|_| Subchannel::new()),

            num_choked: 0,
        }
    }

    /// Read a single packet and returns the messages that were received.
    pub fn read_packet(
        &mut self,
        packet: &[u8],
    ) -> Result<(Vec<NetMessage>, Vec<ReceivedFile>), NetChannelError> {
        let mut reader = BitReader::endian(Cursor::new(packet), LittleEndian);
        let mut messages = vec![];
        let mut files = vec![];

        let flags = self.read_header(&mut reader, packet)?;
        if flags.contains(PacketFlags::RELIABLE) {
            self.read_reliable(&mut reader)?;
        }

        self.read_completed_incoming_transfers(&mut messages, &mut files)?;
        read_messages(&mut reader, self.side, &mut messages)?;

        Ok((messages, files))
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
        trace!(
            "got packet with sequence {sequence}, acked sequence {sequence_ack}, flags {flags:?}"
        );

        let checksum = reader.read_in::<16, u16>()?;
        validate_checksum(checksum, &packet[CHECKSUM_OFFSET + 2..])?;

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

        self.update_subchannel_acks(reliable_state_ack, sequence_ack)?;

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
    fn update_subchannel_acks(
        &mut self,
        reliable_state_ack: u8,
        sequence_ack: i32,
    ) -> Result<(), NetChannelError> {
        for (subchannel_index, subchannel) in self.subchannels.iter_mut().enumerate() {
            let acknowledged_state = (reliable_state_ack & (1 << subchannel_index)) != 0;
            subchannel.update(
                &mut self.out_reliable_transfers,
                acknowledged_state,
                sequence_ack,
            )?;
        }
        Ok(())
    }

    /// Remove completed outgoing transfers from the queue
    fn pop_completed_outgoing_transfers(&mut self) {
        for stream in StreamType::iter() {
            let Some(transfer) = self.out_reliable_transfers.stream(stream).front() else {
                continue;
            };

            if transfer.acked_fragments() == transfer.total_fragments() {
                trace!("transfer for stream {stream:?} is complete");
                self.out_reliable_transfers.stream_mut(stream).pop_front();
            }
        }
    }

    fn read_reliable(&mut self, reader: &mut impl BitRead) -> Result<(), NetChannelError> {
        let subchannel_index = reader.read_in::<3, u8>()?;

        for stream in StreamType::iter() {
            if reader.read_bit()? {
                trace!("got data for stream {stream:?}");
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
            num_fragments = reader.read_in::<{ SUBCHANNEL_FRAGMENT_COUNT_BITS }, u32>()?;
        }

        // Start of the transfer, read the header
        if start_fragment == 0 {
            *self.in_reliable_transfers.stream_mut(stream) = Some(
                IncomingReliableTransfer::new_from_header(reader, is_multi_block)?,
            );
        }

        let transfer = self
            .in_reliable_transfers
            .stream_mut(stream)
            .as_mut()
            .ok_or(NetChannelError::NoActiveTransfer(stream))?;

        transfer.read_data(reader, start_fragment, num_fragments)?;

        Ok(())
    }

    /// Read any completed reliable message transfers.
    fn read_completed_incoming_transfers(
        &mut self,
        messages: &mut Vec<NetMessage>,
        files: &mut Vec<ReceivedFile>,
    ) -> Result<(), NetChannelError> {
        for stream in StreamType::iter() {
            let Some(transfer) = self.in_reliable_transfers.stream_mut(stream) else {
                continue;
            };

            if !transfer.completed() {
                continue;
            }

            trace!("incoming transfer for stream {stream:?} completed");
            let transfer = self.in_reliable_transfers.stream_mut(stream).take().expect(
                "transfer is Some() but take returned None! this should never ever happen!",
            );

            let (transfer_type, data) = transfer.data()?;
            match transfer_type {
                TransferType::Message => {
                    let mut reader = BitReader::endian(Cursor::new(&data), LittleEndian);
                    read_messages(&mut reader, self.side, messages)?
                }
                TransferType::File {
                    transfer_id,
                    filename,
                } => files.push(ReceivedFile {
                    transfer_id,
                    filename,
                    data,
                }),
            }
        }
        Ok(())
    }

    /// Write a single packet. Any messages in `messages` will be sent as
    /// unreliable messages in the packet.
    pub fn write_packet(&mut self, messages: &[NetMessage]) -> Result<Vec<u8>, NetChannelError> {
        let mut writer = BitWriter::endian(Cursor::new(Vec::<u8>::new()), LittleEndian);
        let mut flags = self.write_header(&mut writer)?;

        if self.write_reliable(&mut writer)? {
            flags |= PacketFlags::RELIABLE;
        }

        write_messages(&mut writer, messages)?;
        writer.byte_align()?;

        let mut packet_bytes = writer.into_writer().into_inner();

        // Fill in flags & checksum
        packet_bytes[FLAGS_OFFSET] = flags.bits();
        write_checksum(&mut packet_bytes);

        self.num_choked = 0;
        self.out_sequence_nr += 1;

        Ok(packet_bytes)
    }

    /// Write out the packet header. Dummy values are written to the packet
    /// flags and checksum, they *must* be properly filled in later.
    fn write_header(&mut self, writer: &mut impl BitWrite) -> Result<PacketFlags, NetChannelError> {
        let mut flags = PacketFlags::empty();

        writer.write_out::<32, i32>(self.out_sequence_nr)?;
        writer.write_out::<32, i32>(self.in_sequence_nr)?;

        // Write out a dummy flags value, will be filled in later.
        writer.write_out::<8, u8>(0xaa)?;

        // Write out a dummy checksum value, will be filled in later.
        writer.write_out::<16, u16>(0x5555)?;

        writer.write_out::<8, u8>(self.in_reliable_state)?;

        if self.num_choked > 0 {
            flags |= PacketFlags::CHOKED;
            // Intentionally truncated
            writer.write_out::<8, u8>(self.num_choked as u8)?;
        }

        // Always write a challenge value
        flags |= PacketFlags::CHALLENGE;
        writer.write_out::<32, u32>(self.challenge)?;

        Ok(flags)
    }

    /// Write out the reliable data for a packet. Returns `true` if data has
    /// been written.
    fn write_reliable(&mut self, writer: &mut impl BitWrite) -> Result<bool, NetChannelError> {
        self.fill_free_subchannel()?;

        let Some(subchannel_index) = self.find_subchannel(SubchannelStateType::WaitingToSend)
        else {
            return Ok(false);
        };
        let subchannel = &mut self.subchannels[subchannel_index];
        subchannel.write(
            writer,
            &self.out_reliable_transfers,
            subchannel_index as u8,
            self.out_sequence_nr,
        )?;

        Ok(true)
    }

    fn fill_free_subchannel(&mut self) -> Result<(), NetChannelError> {
        let Some(subchannel_index) = self.find_subchannel(SubchannelStateType::Free) else {
            return Ok(());
        };

        let subchannel = &mut self.subchannels[subchannel_index];
        subchannel.fill(&mut self.out_reliable_transfers, SUBCHANNEL_MAX_SEND_SIZE)?;

        Ok(())
    }

    /// Returns the index of the first subchannel with the desired state.
    fn find_subchannel(&self, state: SubchannelStateType) -> Option<usize> {
        self.subchannels
            .iter()
            .enumerate()
            .find(|(_, s)| s.state() == state)
            .map(|(i, _)| i)
    }

    /// Queue reliable messages to be sent.
    pub fn queue_reliable_messages(&mut self, messages: &[NetMessage]) -> Result<(), NetChannelError> {
        let mut data = vec![];
        let mut writer = BitWriter::endian(Cursor::new(&mut data), LittleEndian);

        write_messages(&mut writer, messages)?;
        writer.byte_align()?;

        self.queue_reliable_transfer(StreamType::Message, TransferType::Message, data)?;

        Ok(())
    }

    /// Queue a reliable transfer to be sent.
    pub fn queue_reliable_transfer(
        &mut self,
        stream: StreamType,
        transfer_type: TransferType,
        data: Vec<u8>,
    ) -> Result<(), NetChannelError> {
        let transfer = OutgoingReliableTransfer::new(transfer_type, data)?;

        self.out_reliable_transfers
            .stream_mut(stream)
            .push_back(transfer);

        Ok(())
    }
}

/// Validate that `checksum` matches the checksum for `data`. Returns Ok(()) if
/// they match.
fn validate_checksum(checksum: u16, data: &[u8]) -> Result<(), NetChannelError> {
    let calculated_checksum = calculate_checksum(data);
    // trace!(
    //     "calculated packet checksum {calculated_checksum:04x}, expected checksum is {checksum:04x}"
    // );

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

/// Calculate a checksum for the all data after `CHECKSUM_OFFSET + 2`, and
/// inserts that checksum at `CHECKSUM_OFFSET`. `packet_bytes` must be at least
/// `CHECKSUM_OFFSET + 2` bytes long, or the function will panic.
fn write_checksum(packet_bytes: &mut [u8]) {
    let bytes_to_checksum = &packet_bytes[CHECKSUM_OFFSET + 2..];
    let checksum = calculate_checksum(bytes_to_checksum);

    let checksum_bytes = checksum.to_le_bytes();
    packet_bytes[CHECKSUM_OFFSET] = checksum_bytes[0];
    packet_bytes[CHECKSUM_OFFSET + 1] = checksum_bytes[1];
}

#[cfg(test)]
mod tests {
    use io_util::{write_string, write_varint32};

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
        assert_eq!(
            calculate_reliable_data_range(0, 1, 4, 1),
            LengthRange::new(0, 4),
            "chunks for transfers smaller than a fragment should not take an entire fragment"
        );
        assert_eq!(
            calculate_reliable_data_range(1, 1, FRAGMENT_SIZE + 4, 2),
            LengthRange::new(FRAGMENT_SIZE, 4),
            "tailing chunks should not take an entire fragment"
        );
        assert_eq!(
            calculate_reliable_data_range(0, 1, FRAGMENT_SIZE + 4, 2),
            LengthRange::new(0, FRAGMENT_SIZE),
            "beginning chunks should take an entire fragment"
        );
    }

    #[test]
    fn test_write_checksum() {
        let mut buf: Vec<u8> = vec![];
        for _ in 0..CHECKSUM_OFFSET {
            buf.push(0x0F);
        }

        // Dummy checksum
        buf.push(0xAA);
        buf.push(0xBB);

        // calculated using https://www.crccalc.com/?crc=01020304&method=CRC-32%2FISO-HDLC&datatype=1&outtype=0
        const EXPECTED_CHECKSUM: u16 = 0xb63c ^ 0xfbcd;
        for i in 1..=4 {
            buf.push(i);
        }

        write_checksum(&mut buf);
        let written_checksum = u16::from_le_bytes(
            buf[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 2]
                .try_into()
                .unwrap(),
        );

        assert_eq!(EXPECTED_CHECKSUM, written_checksum);
    }

    #[test]
    fn test_reliable_read_header_single_block() {
        let mut writer = BitWriter::endian(Cursor::new(vec![]), LittleEndian);

        writer.write_bit(true).unwrap(); // is compressed
        writer.write_out::<MAX_FILE_SIZE_BITS, u32>(5678).unwrap(); // uncompressed size

        write_varint32(&mut writer, 1234).unwrap(); // size

        writer.byte_align().unwrap();

        let data = writer.into_writer().into_inner();
        let mut reader = BitReader::endian(Cursor::new(&data), LittleEndian);

        let transfer = IncomingReliableTransfer::new_from_header(&mut reader, false).unwrap();
        assert_eq!(transfer.buffer.len(), 1234); // buffer size should be compressed size
        assert_eq!(transfer.size, 1234);
        assert_eq!(transfer.uncompressed_size, Some(5678));
        assert!(!transfer.multi_block);
    }

    #[test]
    fn test_reliable_read_header_multi_block() {
        let mut writer = BitWriter::endian(Cursor::new(vec![]), LittleEndian);

        writer.write_bit(true).unwrap(); // is file
        writer.write_out::<32, u32>(9999).unwrap();
        const FILENAME: &str = "very real filename.txt";
        write_string(&mut writer, FILENAME).unwrap();

        writer.write_bit(true).unwrap(); // is compressed
        writer.write_out::<MAX_FILE_SIZE_BITS, u32>(5678).unwrap(); // uncompressed size

        writer.write_out::<MAX_FILE_SIZE_BITS, u32>(1234).unwrap(); // size

        writer.byte_align().unwrap();

        let data = writer.into_writer().into_inner();
        let mut reader = BitReader::endian(Cursor::new(&data), LittleEndian);

        let transfer = IncomingReliableTransfer::new_from_header(&mut reader, true).unwrap();
        assert_eq!(transfer.buffer.len(), 1234); // buffer size should be compressed size
        assert_eq!(transfer.size, 1234);
        assert_eq!(transfer.uncompressed_size, Some(5678));
        assert!(transfer.multi_block);
        assert_eq!(
            transfer.transfer_type,
            TransferType::File {
                transfer_id: 9999,
                filename: FILENAME.to_string()
            }
        );
    }

    #[test]
    fn test_reliable_read_data_single_block() {
        const BUFFER_SIZE: u16 = 1234;
        let mut transfer = IncomingReliableTransfer {
            buffer: vec![0u8; BUFFER_SIZE.into()],
            size: BUFFER_SIZE.into(),
            uncompressed_size: None,
            multi_block: false,
            transfer_type: TransferType::Message,
            received_fragments: 0,
        };

        let mut test_buffer = vec![];
        for i in 0..BUFFER_SIZE {
            test_buffer.push(i as u8);
        }

        let mut reader = BitReader::endian(Cursor::new(&test_buffer), LittleEndian);

        transfer.read_data(&mut reader, 0, 0).unwrap();

        assert_eq!(
            transfer.data().unwrap(),
            (TransferType::Message, test_buffer)
        )
    }

    #[test]
    fn test_reliable_read_data_multi_block() {
        const BUFFER_SIZE: u16 = 1234;
        let mut transfer = IncomingReliableTransfer {
            buffer: vec![0u8; BUFFER_SIZE.into()],
            size: BUFFER_SIZE.into(),
            uncompressed_size: None,
            multi_block: true,
            transfer_type: TransferType::Message,
            received_fragments: 0,
        };

        let mut test_buffer = vec![];
        for i in 0..BUFFER_SIZE {
            test_buffer.push(i as u8);
        }

        let mut reader = BitReader::endian(Cursor::new(&test_buffer), LittleEndian);

        // Send the transfer in 2 chunks
        let half_fragments = bytes_to_fragments(BUFFER_SIZE.into()) / 2;
        let remaining_fragments =
            bytes_to_fragments(u32::from(BUFFER_SIZE) - (half_fragments * FRAGMENT_SIZE));
        transfer.read_data(&mut reader, 0, half_fragments).unwrap();
        transfer
            .read_data(&mut reader, half_fragments, remaining_fragments)
            .unwrap();

        assert_eq!(
            transfer.data().unwrap(),
            (TransferType::Message, test_buffer)
        )
    }
}
