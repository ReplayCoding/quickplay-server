//! An implementation of Source Engine NetChannels
//!
//! NetChannels are the primary way that Source communicates over the network.
//! They are a layer on top of UDP that can send and receive unreliable messages
//! along with reliable messages and file streams.
//!
//! # Reliable Streams
//! Two reliable transfers can occur at one time, a stream
//! for messages and a stream for file transfers. Packets that contain reliable
//! data will have the PACKET_FLAG_RELIABLE flag set.

//! If a packet contains reliable data, it will then specify which of the 8
//! possible subchannels it is sending data on. After the client is finished
//! reading the incoming stream data, it will flip a bit in it's incoming
//! reliable state. The incoming reliable state is sent back to the server. If
//! the client's incoming reliable state doesn't match up with the server's
//! outgoing reliable state, then the server will consider that data to have
//! been dropped and will resend the subchannel data. The client will not
//! receive new data on a subchannel until the previous data has been acked for
//! that subchannel.
use std::{collections::VecDeque, io::Cursor};

use anyhow::{anyhow, Ok};
use bitflags::bitflags;
use bitstream_io::{BitRead, BitReader, BitWrite, BitWriter, LittleEndian};
use tracing::{instrument, trace};

use crate::{
    io_util::{read_varint32, write_varint32},
    message::Message,
};

// TODO: what is the optimal value for us? We probably aren't
// ever going to send 5000 packets
const MAX_PACKETS_DROPPED: u32 = 5000;

const MAX_STREAMS: usize = 2; // 0 == regular, 1 == file stream
const MAX_SUBCHANNELS: usize = 8;

const FRAGMENT_BITS: u32 = 8;
const FRAGMENT_SIZE: u32 = 1 << FRAGMENT_BITS;

const MAX_RELIABLE_PAYLOAD_SIZE: u32 = 1024 /* 288000 */;

const MAX_FILE_SIZE_BITS: u32 = 26;

// Also used by Message::write
pub const NETMSG_TYPE_BITS: u32 = 6;

const MIN_PACKET_SIZE: usize = 16;

bitflags! {
    #[derive(Debug)]
    struct PacketFlags: u8 {
        const PACKET_FLAG_RELIABLE    = 1 << 0; // packet contains subchannel stream data
        const PACKET_FLAG_COMPRESSED  = 1 << 1; // packet is compressed - UNUSED?
        const PACKET_FLAG_ENCRYPTED   = 1 << 2; // packet is encrypted
        const PACKET_FLAG_SPLIT       = 1 << 3; // packet is split - UNUSED?
        const PACKET_FLAG_CHOKED      = 1 << 4; // packet was choked by sender
        const PACKET_FLAG_CHALLENGE   = 1 << 5; // packet contains challenge number, use to prevent packet injection
    }
}

#[derive(Clone, Debug)]
struct IncomingReliableData {
    /// The number of acknowledged fragments. When `acked_fragments` ==
    /// `self.fragments()`, the transfer is complete
    acked_fragments: u32,
    /// The total number of bytes that will be sent to us
    bytes: u32,
    data: Vec<u8>,

    /// Used for file transfers. If this is None then the data contains messages, not a file.
    filename: Option<String>,
}

impl IncomingReliableData {
    fn new(filename: Option<String>, bytes: u32) -> anyhow::Result<IncomingReliableData> {
        let data_len = usize::try_from(bytes)?;
        Ok(Self {
            acked_fragments: 0,
            bytes,
            data: vec![0; data_len],
            filename,
        })
    }

    fn total_fragments(&self) -> u32 {
        bytes_to_fragments(self.bytes)
    }
}

struct OutgoingReliableData {
    acked_fragments: u32,
    pending_fragments: u32,
    data: Vec<u8>,
}
impl OutgoingReliableData {
    fn new(data: Vec<u8>) -> anyhow::Result<Self> {
        if u32::try_from(data.len()).is_err() {
            return Err(anyhow!("outgoing reliable data size does not fit into u32"));
        };

        Ok(Self {
            acked_fragments: 0,
            pending_fragments: 0,
            data,
        })
    }

    fn total_fragments(&self) -> u32 {
        bytes_to_fragments(self.total_bytes())
    }

    fn total_bytes(&self) -> u32 {
        // Data len is bounds checked on creation, so this is ok
        self.data.len() as u32
    }
}

fn bytes_to_fragments(bytes: u32) -> u32 {
    (bytes + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE
}

#[derive(PartialEq, Eq)]
enum SubChannelState {
    /// The subchannel is not currently transferring any data.
    Free,
    /// The subchannel has been filled with data for a transfer, but that data
    /// has not been sent.
    WaitingToSend,
    /// The subchannel has sent data to the client, but it has not received an
    /// acknowledgement that the data has been received.
    WaitingForAck,
}
struct SubChannel {
    state: SubChannelState,

    start_fragment: [u32; MAX_STREAMS],
    num_fragments: [u32; MAX_STREAMS],

    send_seq_nr: i64,
}
impl SubChannel {
    fn new_free() -> SubChannel {
        Self {
            state: SubChannelState::Free,
            start_fragment: [0; MAX_STREAMS],
            num_fragments: [0; MAX_STREAMS],
            send_seq_nr: -1,
        }
    }
}

pub struct NetChannel {
    /// Current outgoing sequence number
    out_sequence_nr: u32,
    /// Last outgoing sequence number that has been acknowledged by the remote
    out_sequence_nr_ack: u32,

    /// Highest seen incoming sequence number
    in_sequence_nr: u32,

    has_seen_challenge: bool,
    challenge: u32,

    in_reliable_state: u8,
    incoming_reliable_data: [Option<IncomingReliableData>; MAX_STREAMS],

    out_reliable_state: u8,
    outgoing_reliable_data: [VecDeque<OutgoingReliableData>; MAX_STREAMS],
    outgoing_subchannels: [SubChannel; MAX_SUBCHANNELS],
}

impl NetChannel {
    pub fn new(challenge: u32) -> Self {
        Self {
            out_sequence_nr: 1,
            out_sequence_nr_ack: 0,

            in_sequence_nr: 0,

            has_seen_challenge: false,
            challenge,

            in_reliable_state: 0,
            incoming_reliable_data: std::array::from_fn(|_| None),

            out_reliable_state: 0,
            outgoing_reliable_data: std::array::from_fn(|_| VecDeque::new()),
            outgoing_subchannels: std::array::from_fn(|_| SubChannel::new_free()),
        }
    }

    #[instrument(skip_all)]
    pub fn process_packet(&mut self, packet: &[u8]) -> anyhow::Result<Vec<Message>> {
        let mut reader = BitReader::endian(Cursor::new(packet), LittleEndian);
        let flags = self.parse_header(&mut reader, packet)?;

        let mut messages = vec![];

        if flags.contains(PacketFlags::PACKET_FLAG_RELIABLE) {
            let subchannel_bit: u8 = reader.read_in::<3, _>()?;

            for i in 0..MAX_STREAMS {
                if reader.read_bit()? {
                    self.read_incoming_subchannel_data(&mut reader, i)?;
                }
            }

            self.in_reliable_state ^= 1 << subchannel_bit;
            trace!(
                "flipped subchannel {subchannel_bit} (new in_reliable_state {:08b})",
                self.in_reliable_state
            );

            for i in 0..MAX_STREAMS {
                self.process_incoming_subchannel_data(i, &mut messages)?;
            }
        }

        self.process_messages(&mut reader, packet.len(), &mut messages)?;

        Ok(messages)
    }

    #[instrument(skip_all)]
    fn parse_header<R, E>(
        &mut self,
        reader: &mut BitReader<R, E>,
        packet: &[u8],
    ) -> anyhow::Result<PacketFlags>
    where
        R: std::io::Read + std::io::Seek,
        E: bitstream_io::Endianness,
    {
        let sequence: u32 = reader.read_in::<32, _>()?;
        let sequence_ack: u32 = reader.read_in::<32, _>()?;
        let flags: PacketFlags = PacketFlags::from_bits_truncate(reader.read_in::<8, u8>()?);

        trace!("sequence {sequence} sequence_ack {sequence_ack} flags {flags:?}");

        {
            let expected_checksum: u16 = reader.read_in::<16, _>()?;

            // Since we're grabbing the bytes from the original slice, we need
            // to make sure that the current BitReader position is aligned.
            debug_assert!(reader.byte_aligned(), "message reader is not byte aligned");

            let bytes_to_checksum = &packet[usize::try_from(reader.position_in_bits()? / 8)?..];
            let calculated_checksum = calculate_checksum(bytes_to_checksum);

            if calculated_checksum != expected_checksum {
                return Err(anyhow!(
                    "checksum mismatch: {:04x} != {:04x}",
                    calculated_checksum,
                    expected_checksum
                ));
            }
        }

        let reliable_state: u8 = reader.read_in::<8, _>()?;
        let mut n_choked: u32 = 0;

        if flags.contains(PacketFlags::PACKET_FLAG_CHOKED) {
            n_choked = reader.read_in::<8, _>()?;
        };

        if flags.contains(PacketFlags::PACKET_FLAG_CHALLENGE) {
            let challenge: u32 = reader.read_in::<32, _>()?;
            if challenge != self.challenge {
                return Err(anyhow!(
                    "challenge mismatch: {:08x} != {:08x}",
                    challenge,
                    self.challenge
                ));
            }

            self.has_seen_challenge = true;
        } else if self.has_seen_challenge {
            return Err(anyhow!("challenge value expected, but not given"));
        };

        if sequence <= self.in_sequence_nr {
            return Err(anyhow!(
                "unexpected sequence number {sequence}, our in_sequence_nr is {}",
                self.in_sequence_nr
            ));
        }

        let num_packets_dropped = sequence - (self.in_sequence_nr + n_choked + 1);
        if num_packets_dropped > 0 {
            trace!("dropped {num_packets_dropped} packets");
        }

        if num_packets_dropped > MAX_PACKETS_DROPPED {
            return Err(anyhow!("number of packets dropped ({num_packets_dropped}) has exceeded maximum allowed ({MAX_PACKETS_DROPPED})"));
        }

        self.update_outgoing_subchannel_ack(sequence_ack, reliable_state)?;

        self.in_sequence_nr = sequence;
        self.out_sequence_nr_ack = sequence_ack;

        self.check_outgoing_subchannel_completion()?;

        Ok(flags)
    }

    fn update_outgoing_subchannel_ack(
        &mut self,
        sequence_ack: u32,
        acked_reliable_state: u8,
    ) -> anyhow::Result<()> {
        trace!("outgoing reliable ack {acked_reliable_state:08b}");
        for subchannel_index in 0..MAX_SUBCHANNELS {
            assert!(MAX_SUBCHANNELS <= 8);
            let reliable_state_mask = 1 << subchannel_index as u8;
            let subchannel = &mut self.outgoing_subchannels[subchannel_index];

            if (self.out_reliable_state & reliable_state_mask)
                == (acked_reliable_state & reliable_state_mask)
            {
                if subchannel.send_seq_nr > sequence_ack.into() {
                    return Err(anyhow!(
                        "invalid reliable state for subchannel {subchannel_index} ({} > {})",
                        subchannel.send_seq_nr,
                        sequence_ack
                    ));
                } else if subchannel.state == SubChannelState::WaitingForAck {
                    for (streams_idx, streams) in self.outgoing_reliable_data.iter_mut().enumerate()
                    {
                        if subchannel.num_fragments[streams_idx] == 0 {
                            continue;
                        }

                        let stream = streams
                            .front_mut()
                            .ok_or_else(|| anyhow!("subchannel waiting for ack, but no stream!"))?;

                        stream.acked_fragments += subchannel.num_fragments[streams_idx];
                        stream.pending_fragments -= subchannel.num_fragments[streams_idx];
                        trace!(
                            "updated stream {}: acked frags {}",
                            streams_idx,
                            stream.acked_fragments
                        );
                    }

                    *subchannel = SubChannel::new_free();
                }
            } else {
                if subchannel.send_seq_nr <= sequence_ack.into() {
                    if subchannel.state == SubChannelState::Free {
                        return Err(anyhow!("subchannel should not be free here!"));
                    }

                    if subchannel.state == SubChannelState::WaitingForAck {
                        subchannel.state = SubChannelState::WaitingToSend;

                        trace!("resending subchannel {}", subchannel_index);
                    }
                }
            }
        }
        Ok(())
    }

    fn check_outgoing_subchannel_completion(&mut self) -> anyhow::Result<()> {
        for (streams_idx, streams) in self.outgoing_reliable_data.iter_mut().enumerate() {
            let mut stream_completed = false;
            if let Some(stream) = streams.front() {
                if stream.acked_fragments == stream.total_fragments() {
                    stream_completed = true;
                }
            }

            if stream_completed {
                trace!("completed sending stream {}", streams_idx);
                streams.pop_front();
            }
        }
        Ok(())
    }

    #[instrument(skip_all)]
    fn read_incoming_subchannel_data<R, E>(
        &mut self,
        reader: &mut BitReader<R, E>,
        stream: usize,
    ) -> anyhow::Result<()>
    where
        R: std::io::Read + std::io::Seek,
        E: bitstream_io::Endianness,
    {
        let is_multi_block = reader.read_bit()?;
        // trace!("is_multi_block {is_multi_block} [i {stream}]");

        let mut start_fragment = 0;
        let mut num_fragments = 0;
        let mut offset = 0;
        let mut length = 0;

        if is_multi_block {
            start_fragment = reader.read_in::<{ MAX_FILE_SIZE_BITS - FRAGMENT_BITS }, _>()?;
            num_fragments = reader.read_in::<3, _>()?;

            offset = start_fragment * FRAGMENT_SIZE;
            length = num_fragments * FRAGMENT_SIZE;
        }

        // trace!("start_fragment {start_fragment} num_fragments {num_fragments} offset {offset} length {length}");

        // Start of subchannel data, let's read the header
        if offset == 0 {
            let bytes: u32;

            if is_multi_block {
                // is file?
                if reader.read_bit()? {
                    return Err(anyhow!("file transfer"));
                }

                // NOTE: The client will only compress streams if the server
                // sends a ServerInfo packet with m_nMaxClients > 0.
                // is compressed?
                if reader.read_bit()? {
                    return Err(anyhow!("compressed data"));
                }

                bytes = reader.read_in::<MAX_FILE_SIZE_BITS, _>()?;
            } else {
                // is compressed?
                if reader.read_bit()? {
                    return Err(anyhow!("compressed data"));
                }

                bytes = read_varint32(reader)?;
            }

            let received_data = IncomingReliableData::new(None, bytes)?;
            if !is_multi_block {
                num_fragments = received_data.total_fragments();
                length = num_fragments * FRAGMENT_SIZE;
            }

            // TODO: check that the size isn't too large

            self.incoming_reliable_data[stream] = Some(received_data);
        };

        let received_data = self.incoming_reliable_data[stream]
            .as_mut()
            .ok_or_else(|| anyhow!("no active subchannel data"))?;

        if (start_fragment + num_fragments) == received_data.total_fragments() {
            // we are receiving the last fragment, adjust length
            let rest = FRAGMENT_SIZE - (received_data.bytes % FRAGMENT_SIZE);
            // trace!("rest {rest}");

            if rest < FRAGMENT_SIZE {
                length -= rest;
            }
        };

        if (start_fragment + num_fragments) > received_data.total_fragments() {
            return Err(anyhow!(
                "Received out-of-bounds fragment: {} + {} > {}",
                start_fragment,
                num_fragments,
                received_data.total_fragments()
            ));
        }

        // trace!(
        //     "length {} num_fragments {} bytes {}",
        //     length,
        //     num_fragments,
        //     received_data.bytes
        // );

        let offset = usize::try_from(offset)?;
        let length = usize::try_from(length)?;
        let fragment_slice = received_data
            .data
            .get_mut(offset..offset + length)
            .ok_or_else(|| anyhow!("couldn't get slice for fragment"))?;
        reader.read_bytes(fragment_slice)?;

        received_data.acked_fragments += num_fragments;

        Ok(())
    }

    #[instrument(skip_all)]
    fn process_incoming_subchannel_data(
        &mut self,
        stream: usize,
        messages: &mut Vec<Message>,
    ) -> anyhow::Result<()> {
        let received_data = &self.incoming_reliable_data[stream];
        if let Some(received_data) = received_data {
            if received_data.acked_fragments < received_data.total_fragments() {
                // Haven't got all the data yet
                return Ok(());
            }

            if received_data.acked_fragments > received_data.total_fragments() {
                return Err(anyhow!(
                    "Subchannel fragments overflow: {} > {}",
                    received_data.acked_fragments,
                    received_data.total_fragments()
                ));
            }

            // TODO: handle compressed

            if received_data.filename.is_none() {
                let mut reader = BitReader::endian(Cursor::new(&received_data.data), LittleEndian);
                self.process_messages(&mut reader, received_data.data.len(), messages)?;
            } else {
                return Err(anyhow!("file upload"));
            }

            // Done receiving data, reset subchannel
            self.incoming_reliable_data[stream] = None;
        }

        Ok(())
    }

    #[instrument(skip_all)]
    fn process_messages<R, E>(
        &self,
        reader: &mut BitReader<R, E>,
        data_len: usize,
        messages: &mut Vec<Message>,
    ) -> anyhow::Result<()>
    where
        R: std::io::Read + std::io::Seek,
        E: bitstream_io::Endianness,
    {
        loop {
            if (u64::try_from(data_len)? * 8) - reader.position_in_bits()? < NETMSG_TYPE_BITS.into()
            {
                // No more messages
                break;
            }

            let message_type: u32 = reader.read_in::<NETMSG_TYPE_BITS, _>()?;
            let message = Message::read(reader, message_type)?;

            messages.push(message);
        }

        Ok(())
    }

    // TODO: this should take some sort of socket wrapper that handles packet
    // splitting and compression
    #[instrument(skip_all)]
    pub fn create_send_packet(&mut self, messages: &[Message]) -> anyhow::Result<Vec<u8>> {
        let mut buffer: Vec<u8> = vec![];
        let mut writer = BitWriter::endian(Cursor::new(&mut buffer), LittleEndian);

        let mut flags = PacketFlags::empty();

        writer.write_out::<32, _>(self.out_sequence_nr)?;
        writer.write_out::<32, _>(self.in_sequence_nr)?;

        let flags_offset: usize = (32 + 32) / 8;
        writer.write_out::<8, _>(0)?; // write out dummy flags

        // NOTE: this is really Stupid, i shouldn't have to manually calculate this.
        let checksum_offs: usize = (32 + 32 + 8) / 8;
        writer.write_out::<16, _>(0)?; // write out dummy checksum

        writer.write_out::<8, _>(self.in_reliable_state)?;

        // always write out challenge
        flags |= PacketFlags::PACKET_FLAG_CHALLENGE;
        writer.write_out::<32, _>(self.challenge)?;

        if self.send_outgoing_subchannel_data(&mut writer)? {
            flags |= PacketFlags::PACKET_FLAG_RELIABLE;
        }

        for message in messages {
            message.write(&mut writer)?;
        }

        // pad out data so everything is written. this *should* be fine to use,
        // since it should translate to a net_NOP at worst. i think. hopefully.
        writer.byte_align()?;

        // apparently, some routers don't like packets that are too small
        if buffer.len() < MIN_PACKET_SIZE {
            buffer.resize(MIN_PACKET_SIZE, 0);
        }

        // write flags into buffer now
        buffer[flags_offset] = flags.bits();

        // should never panic because we've already written the dummy checksum.
        // at worst we get an empty slice
        let bytes_to_checksum = &buffer[checksum_offs + 2..];
        let checksum = calculate_checksum(bytes_to_checksum).to_le_bytes();

        buffer[checksum_offs] = checksum[0];
        buffer[checksum_offs + 1] = checksum[1];

        self.out_sequence_nr += 1;

        trace!(
            "sending packet [out seq nr {}] [in seq nr {}] [in reliable state {:08b}] [flags {:?}]",
            self.out_sequence_nr,
            self.in_sequence_nr,
            self.in_reliable_state,
            flags
        );

        Ok(buffer)
    }

    #[instrument(skip_all)]
    fn send_outgoing_subchannel_data<W: BitWrite>(
        &mut self,
        writer: &mut W,
    ) -> anyhow::Result<bool> {
        self.update_outgoing_subchannels()?;

        // Find a subchannel that we can send
        let subchannel = self
            .outgoing_subchannels
            .iter_mut()
            .enumerate()
            .find(|(_, s)| s.state == SubChannelState::WaitingToSend);

        if let Some((subchannel_index, subchannel)) = subchannel {
            assert!(MAX_SUBCHANNELS <= 8);
            writer.write_out::<3, _>(subchannel_index as u8)?;

            for (streams_idx, streams) in self.outgoing_reliable_data.iter_mut().enumerate() {
                if let Some(stream) = streams.front_mut() {
                    writer.write_bit(true)?;

                    let offset = subchannel.start_fragment[streams_idx] * FRAGMENT_SIZE;
                    let mut length = subchannel.num_fragments[streams_idx] * FRAGMENT_SIZE;

                    if (subchannel.start_fragment[streams_idx]
                        + subchannel.num_fragments[streams_idx])
                        == stream.total_fragments()
                    {
                        // we are sending the last fragment, adjust length
                        let rest = FRAGMENT_SIZE - (stream.total_bytes() % FRAGMENT_SIZE);

                        if rest < FRAGMENT_SIZE {
                            length -= rest;
                        }
                    };

                    let is_single_block =
                        subchannel.num_fragments[streams_idx] == stream.total_fragments();
                    writer.write_bit(!is_single_block)?;

                    if is_single_block {
                        assert_eq!(length, stream.total_bytes());
                        assert!(length < MAX_RELIABLE_PAYLOAD_SIZE);
                        assert_eq!(offset, 0);

                        // TODO: compression
                        // is compressed bit
                        writer.write_bit(false)?;

                        write_varint32(writer, stream.total_bytes())?;
                    } else {
                        writer.write_out::<{ MAX_FILE_SIZE_BITS - FRAGMENT_BITS }, _>(
                            subchannel.start_fragment[streams_idx],
                        )?;
                        writer.write_out::<3, _>(subchannel.num_fragments[streams_idx])?;

                        if offset == 0 {
                            // TODO: handle file transfers, this will require a bit more work though
                            // is file transfer bit
                            writer.write_bit(false)?;

                            // TODO: compression
                            // is compressed bit
                            writer.write_bit(false)?;

                            writer.write_out::<MAX_FILE_SIZE_BITS, _>(stream.total_bytes())?;
                        }
                    }

                    let offset = usize::try_from(offset)?;
                    let length = usize::try_from(length)?;
                    writer.write_bytes(&stream.data.get(offset..offset + length).ok_or_else(
                        || anyhow!("couldn't get slice for outgoing reliable data"),
                    )?)?;

                    subchannel.state = SubChannelState::WaitingForAck;
                    subchannel.send_seq_nr = self.out_sequence_nr.into();
                } else {
                    // No data for this stream
                    writer.write_bit(false)?;
                }
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }

    #[instrument(skip_all)]
    fn update_outgoing_subchannels(&mut self) -> anyhow::Result<()> {
        let subchannel = self
            .outgoing_subchannels
            .iter_mut()
            .enumerate()
            .find(|(_, s)| s.state == SubChannelState::Free);

        if let Some((subchannel_index, subchannel)) = subchannel {
            let mut send_data = false;

            // max number of fragments that may be sent in one packet
            let mut max_send_fragments: u32 = MAX_RELIABLE_PAYLOAD_SIZE / FRAGMENT_SIZE;

            for (streams_idx, streams) in self.outgoing_reliable_data.iter_mut().enumerate() {
                if let Some(stream) = streams.front_mut() {
                    let sent_fragments = stream.acked_fragments + stream.pending_fragments;

                    if sent_fragments == stream.total_fragments() {
                        // No data left to send
                        continue;
                    };

                    // number of fragments we'll send in this packet
                    let num_fragments: u32 =
                        max_send_fragments.min(stream.total_fragments() - sent_fragments);

                    subchannel.start_fragment[streams_idx] = sent_fragments;
                    subchannel.num_fragments[streams_idx] = num_fragments;
                    stream.pending_fragments += num_fragments;
                    send_data = true;

                    if let Some(new_max_send_fragments) =
                        max_send_fragments.checked_sub(num_fragments)
                    {
                        max_send_fragments = new_max_send_fragments;
                    } else {
                        // Can't send any more data
                        break;
                    }
                }
            }

            if send_data {
                self.out_reliable_state ^= 1 << subchannel_index;

                subchannel.state = SubChannelState::WaitingToSend;
                subchannel.send_seq_nr = 0;
                trace!("free subchannel {subchannel_index} has been populated, out_reliable_state is now {:08b}", self.out_reliable_state);
            }
        }

        Ok(())
    }

    pub fn queue_reliable_messages(&mut self, messages: &[Message]) -> anyhow::Result<()> {
        let mut data: Vec<u8> = vec![];
        let mut writer = BitWriter::endian(Cursor::new(&mut data), LittleEndian);

        for message in messages {
            message.write(&mut writer)?;
        }

        // Data must be byte-aligned or we won't write it all, pad with nops
        writer.byte_align()?;

        let stream = OutgoingReliableData::new(data)?;
        trace!(
            "created new reliable stream with {} fragments",
            stream.total_fragments()
        );

        // TODO: send reliable file streams too?
        self.outgoing_reliable_data[0].push_back(stream);

        Ok(())
    }
}

fn calculate_checksum(bytes: &[u8]) -> u16 {
    let crc_hasher = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
    let calculated_digest = crc_hasher.checksum(bytes);
    let calculated_checksum = (((calculated_digest >> 16) ^ calculated_digest) & 0xffff) as u16;
    calculated_checksum
}

#[test]
fn test_netchannels() {
    let mut server_channel = NetChannel::new(0);
    let mut client_channel = NetChannel::new(0);

    let messages = [Message::Print(crate::message::MessagePrint {
        text: "0".to_string(),
    })];

    let packet = client_channel.create_send_packet(&messages).unwrap();
    let recieved_messages = server_channel.process_packet(&packet).unwrap();

    assert_eq!(recieved_messages, messages);
}
