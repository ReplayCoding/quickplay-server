use std::io::Cursor;

use anyhow::anyhow;
use bitflags::bitflags;
use bitstream_io::{BitRead, BitReader, LittleEndian};
use log::trace;

use crate::io_util::{read_string, read_varint32};

// TODO: what is the optimal value for us? We probably aren't
// ever going to send 5000 packets
const MAX_PACKETS_DROPPED: u32 = 5000;

const MAX_STREAMS: usize = 2; // 0 == regular, 1 == file stream

const FRAGMENT_BITS: u32 = 8;
const FRAGMENT_SIZE: u32 = 1 << FRAGMENT_BITS;

const NETMSG_TYPE_BITS: u32 = 6;

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
struct ReceivedData {
    acked_fragments: u32,
    bytes: u32,
    data: Vec<u8>,
    filename: Option<String>,
}

impl ReceivedData {
    fn new(
        filename: Option<String>,
        bytes: u32,
    ) -> Result<ReceivedData, std::num::TryFromIntError> {
        let data_len = usize::try_from(bytes)?;
        Ok(Self {
            acked_fragments: 0,
            bytes,
            data: vec![0; data_len],
            filename,
        })
    }

    fn fragments(&self) -> u32 {
        (self.bytes + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE
    }
}

pub struct NetChannel {
    _out_sequence_nr: u32,
    out_sequence_nr_ack: u32,

    in_sequence_nr: u32,

    _out_reliable_state: u32, // we *probably* don't need this
    in_reliable_state: u32,

    has_seen_challenge: bool,
    challenge: u32,

    received_data: [Option<ReceivedData>; MAX_STREAMS],
}

impl NetChannel {
    pub fn new(challenge: u32) -> Self {
        Self {
            _out_sequence_nr: 1,
            out_sequence_nr_ack: 0,

            in_sequence_nr: 0,

            _out_reliable_state: 0,
            in_reliable_state: 0,

            has_seen_challenge: false,
            challenge,

            received_data: std::array::from_fn(|_| None),
        }
    }

    pub fn get_messages(&mut self, packet: &[u8]) -> anyhow::Result<()> {
        let mut reader = BitReader::endian(Cursor::new(packet), LittleEndian);
        let flags = self.parse_header(&mut reader, packet)?;

        if flags.contains(PacketFlags::PACKET_FLAG_RELIABLE) {
            let subchannel_bit: u8 = reader.read_in::<3, _>()?;

            for i in 0..MAX_STREAMS {
                if reader.read_bit()? {
                    self.read_subchannel_data(&mut reader, i)?;
                }
            }

            // TODO: why isn't this set *after* we've successfully parsed the receieved data?
            self.in_reliable_state ^= 1 << subchannel_bit;
            trace!(
                "subchannel_bit {subchannel_bit} reliable_state {}",
                self.in_reliable_state
            );

            for i in 0..MAX_STREAMS {
                self.process_subchannel_data(i)?;
            }
        }

        // FIXME: incorrect calculation i think?
        if ((u64::try_from(packet.len())? * 8) - reader.position_in_bits()?) > 0 {
            trace!(
                "process remaining messages: {} > {}",
                packet.len() * 8,
                reader.position_in_bits()?
            );

            self.process_messages(&mut reader, packet.len())?;
        }

        Ok(())
    }

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

            let crc_hasher = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
            let calculated_digest =
                crc_hasher.checksum(&packet[(reader.position_in_bits()? / 8) as usize..]);
            let calculated_checksum =
                (((calculated_digest >> 16) ^ calculated_digest) & 0xffff) as u16;

            if calculated_checksum != expected_checksum {
                return Err(anyhow!(
                    "checksum mismatch: {:04x} != {:04x}",
                    calculated_checksum,
                    expected_checksum
                ));
            }
        }

        let _reliable_state: u8 = reader.read_in::<8, _>()?;
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

        // TODO: handle subchannel stuff

        self.in_sequence_nr = sequence;
        self.out_sequence_nr_ack = sequence_ack;

        Ok(flags)
    }

    fn read_subchannel_data<R, E>(
        &mut self,
        reader: &mut BitReader<R, E>,
        channel: usize,
    ) -> anyhow::Result<()>
    where
        R: std::io::Read + std::io::Seek,
        E: bitstream_io::Endianness,
    {
        let is_multi_block = reader.read_bit()?;
        trace!("is_multi_block {is_multi_block} [i {channel}]");

        let start_fragment = 0;
        let mut num_fragments = 0;
        let offset = 0;
        let mut length = 0;

        if is_multi_block {
            return Err(anyhow!("multi block"));
        }

        // Start of subchannel data, let's read the header
        if offset == 0 {
            let bytes: u32;

            if is_multi_block {
                return Err(anyhow!("multi block"));
            } else {
                let is_compressed = reader.read_bit()?;
                trace!("\t is_compressed {is_compressed}");
                if is_compressed {
                    return Err(anyhow!("compressed data"));
                }

                bytes = read_varint32(reader)?;
            }

            let received_data = ReceivedData::new(None, bytes)?;
            if !is_multi_block {
                num_fragments = received_data.fragments();
                length = num_fragments * FRAGMENT_SIZE;
            }

            // TODO: check that the size isn't too large

            self.received_data[channel] = Some(received_data);
        };

        let received_data = self.received_data[channel]
            .as_mut()
            .ok_or_else(|| anyhow!("no active subchannel data"))?;

        if (start_fragment + num_fragments) == received_data.fragments() {
            // we are receiving the last fragment, adjust length
            let rest = FRAGMENT_SIZE - (received_data.bytes % FRAGMENT_SIZE);
            trace!("rest {rest}");

            if rest < FRAGMENT_SIZE {
                length -= rest;
            }
        };

        if (start_fragment + num_fragments) > received_data.fragments() {
            return Err(anyhow!(
                "Received out-of-bounds fragment: {} + {} > {}",
                start_fragment,
                num_fragments,
                received_data.fragments()
            ));
        }

        trace!(
            "length {} num_fragments {} bytes {}",
            length,
            num_fragments,
            received_data.bytes
        );

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

    fn process_subchannel_data(&mut self, channel: usize) -> anyhow::Result<()> {
        let received_data = &self.received_data[channel];
        if let Some(received_data) = received_data {
            if received_data.acked_fragments < received_data.fragments() {
                // Haven't got all the data yet
                return Ok(());
            }

            if received_data.acked_fragments > received_data.fragments() {
                return Err(anyhow!(
                    "Subchannel fragments overflow: {} > {}",
                    received_data.acked_fragments,
                    received_data.fragments()
                ));
            }

            // TODO: handle compressed

            if received_data.filename.is_none() {
                let mut reader = BitReader::endian(Cursor::new(&received_data.data), LittleEndian);
                self.process_messages(&mut reader, received_data.data.len())?;
            } else {
                return Err(anyhow!("file upload"));
            }

            // Done receiving data, reset subchannel
            self.received_data[channel] = None;
        }

        Ok(())
    }

    fn process_messages<R: std::io::Read + std::io::Seek, E: bitstream_io::Endianness>(
        &self,
        reader: &mut BitReader<R, E>,
        data_len: usize,
    ) -> anyhow::Result<()> {
        // CRAPPY BROKEN TESTING CODE, REPLACE ME ASAP
        loop {
            if ((u64::try_from(data_len)? * 8) - reader.position_in_bits()?)
                < NETMSG_TYPE_BITS.into()
            {
                break;
            }

            let msg: u32 = reader.read_in::<NETMSG_TYPE_BITS, _>()?;
            match msg {
                1 => {
                    let reason = read_string(reader, 1024)?;
                    trace!("disconnect: {reason}");
                }
                5 => {
                    let num_vars: u8 = reader.read_in::<8, _>()?;

                    for _i in 0..num_vars {
                        let name = read_string(reader, 260)?;
                        let value = read_string(reader, 260)?;

                        trace!("convar {} = {}", name, value);
                    }
                }
                _ => return Err(anyhow!("implement msg {msg}")),
            };
        }

        Ok(())
    }
}
