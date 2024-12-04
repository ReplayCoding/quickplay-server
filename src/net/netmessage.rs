use bitstream_io::{BitRead, BitWrite};
use thiserror::Error;

use crate::io_util::{read_string, write_string};

use super::message::{Message, MessageSide};

pub const NETMSG_TYPE_BITS: u32 = 6; // must be 2^NETMSG_TYPE_BITS > SVC_LASTMSG

#[derive(Error, Debug)]
pub enum NetMessageError {
    #[error("unknown message type {0} receiving on side {1:?}")]
    UnknownMessage(u8, MessageSide),
    #[error("too many convars: {0}")]
    TooManyConVars(usize),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, PartialEq)]
pub struct Nop;

impl Message<NetMessageError> for Nop {
    const TYPE: u8 = 0;
    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        Ok(Self)
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct Disconnect {
    pub reason: String,
}

impl Message<NetMessageError> for Disconnect {
    const TYPE: u8 = 1;
    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        let reason = read_string(reader, 1024)?;
        Ok(Self { reason })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError>
    where
        Self: Sized,
    {
        write_string(writer, &self.reason)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct StringCmd {
    pub command: String,
}

impl Message<NetMessageError> for StringCmd {
    const TYPE: u8 = 4;
    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        let command = read_string(reader, 1024)?;
        Ok(Self { command })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError> {
        write_string(writer, &self.command)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct ConVar {
    pub name: String,
    pub value: String,
}

#[derive(Debug, PartialEq)]
pub struct SetConVars {
    pub convars: Vec<ConVar>,
}

impl Message<NetMessageError> for SetConVars {
    const TYPE: u8 = 5;
    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        let num_convars = reader.read_in::<8, u8>()?;
        let mut convars = vec![];
        for _ in 0..num_convars {
            let name = read_string(reader, 260)?;
            let value = read_string(reader, 260)?;
            convars.push(ConVar { name, value });
        }
        Ok(Self { convars })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError> {
        let num_convars = u8::try_from(self.convars.len())
            .map_err(|_| NetMessageError::TooManyConVars(self.convars.len()))?;

        writer.write_out::<8, u8>(num_convars)?;
        for convar in &self.convars {
            write_string(writer, &convar.name)?;
            write_string(writer, &convar.value)?;
        }

        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct SignonState {
    pub signon_state: u8,
    pub spawn_count: i32,
}

impl Message<NetMessageError> for SignonState {
    const TYPE: u8 = 6;
    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        let signon_state = reader.read_in::<8, u8>()?;
        let spawn_count = reader.read_in::<32, i32>()?;

        Ok(Self {
            signon_state,
            spawn_count,
        })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError> {
        writer.write_out::<8, _>(self.signon_state)?;
        writer.write_out::<32, _>(self.spawn_count)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct Print {
    pub text: String,
}

impl Message<NetMessageError> for Print {
    const TYPE: u8 = 7;
    const SIDE: MessageSide = MessageSide::Server;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        let text = read_string(reader, 2048)?;
        Ok(Self { text })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError> {
        write_string(writer, &self.text)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub enum FileMode {
    Request,
    Deny,
}
#[derive(Debug, PartialEq)]
pub struct File {
    pub mode: FileMode,
    pub filename: String,
    pub transfer_id: u32,
}

impl Message<NetMessageError> for File {
    const TYPE: u8 = 2;
    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        let transfer_id = reader.read_in::<32, u32>()?;
        let filename = read_string(reader, 1024)?;
        let mode = if reader.read_bit()? {
            FileMode::Request
        } else {
            FileMode::Deny
        };

        Ok(Self {
            mode,
            filename,
            transfer_id,
        })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError> {
        writer.write_out::<32, u32>(self.transfer_id)?;
        write_string(writer, &self.filename)?;
        writer.write_bit(self.mode == FileMode::Request)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
enum MapHash {
    Md5([u8; 16]),
    Crc(u32),
}

#[derive(Debug, PartialEq)]
pub struct ServerInfo {
    protocol: u16,
    server_count: u32,
    is_hltv: bool,
    is_dedicated: bool,
    legacy_client_crc: u32,
    max_classes: u16,
    map_hash: MapHash,
    player_slot: u8,
    max_clients: u8,
    tick_interval: f32,
    os: u8,
    game_dir: String,
    map_name: String,
    sky_name: String,
    host_name: String,
    is_replay: bool,
}

impl Message<NetMessageError> for ServerInfo {
    const TYPE: u8 = 8;

    const SIDE: MessageSide = MessageSide::Server;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        let protocol = reader.read_in::<16, u16>()?;
        let server_count = reader.read_in::<32, u32>()?;
        let is_hltv = reader.read_bit()?;
        let is_dedicated = reader.read_bit()?;
        let legacy_client_crc = reader.read_in::<32, u32>()?;
        let max_classes = reader.read_in::<16, u16>()?;

        let map_hash = if protocol > 17 {
            let mut md5 = [0u8; 16];
            reader.read_bytes(&mut md5)?;
            MapHash::Md5(md5)
        } else {
            MapHash::Crc(reader.read_in::<32, u32>()?)
        };

        let player_slot = reader.read_in::<8, u8>()?;
        let max_clients = reader.read_in::<8, u8>()?;
        let tick_interval = f32::from_bits(reader.read_in::<32, u32>()?);
        let os = reader.read_in::<8, u8>()?;

        let game_dir = read_string(reader, 260)?;
        let map_name = read_string(reader, 260)?;
        let sky_name = read_string(reader, 260)?;
        let host_name = read_string(reader, 260)?;

        // FIXME: This has some checks for the netchannel protocol version...
        // Maybe have some way to check here?
        let is_replay = reader.read_bit()?;

        Ok(Self {
            protocol,
            server_count,
            is_hltv,
            is_dedicated,
            legacy_client_crc,
            max_classes,
            map_hash,
            player_slot,
            max_clients,
            tick_interval,
            os,
            game_dir,
            map_name,
            sky_name,
            host_name,
            is_replay,
        })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError> {
        todo!()
    }
}

const NET_TICK_SCALEUP: f32 = 100000.0;

#[derive(Debug, PartialEq)]
pub struct Tick {
    tick: u32,
    host_frametime: f32,
    host_frametime_stddev: f32,
}

impl Message<NetMessageError> for Tick {
    const TYPE: u8 = 3;

    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> Result<Self, NetMessageError>
    where
        Self: Sized,
    {
        let tick = reader.read_in::<32, u32>()?;
        let host_frametime = f32::from(reader.read_in::<16, u16>()?) / NET_TICK_SCALEUP;
        let host_frametime_stddev = f32::from(reader.read_in::<16, u16>()?) / NET_TICK_SCALEUP;

        Ok(Self {
            tick,
            host_frametime,
            host_frametime_stddev,
        })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), NetMessageError> {
        todo!()
    }
}

#[derive(Debug, PartialEq)]
pub enum NetMessage {
    Nop(Nop),
    Disconnect(Disconnect),
    StringCmd(StringCmd),
    SetConVars(SetConVars),
    SignonState(SignonState),
    Print(Print),
    File(File),
    ServerInfo(ServerInfo),
    Tick(Tick),
}

/// Helper to generate the match statement for reading messages
macro_rules! read_messages_match {
    ($reader:ident, $side:ident, $message_type:ident, $($struct:ident => $discriminant:ident), *) => {
        match $message_type {
            $($struct::TYPE if $struct::SIDE.can_receive($side) => NetMessage::$discriminant($struct::read($reader)?),)*
            _ => return Err(NetMessageError::UnknownMessage($message_type, $side))
        }
    };
}

/// Helper to generate the match statement for writing messages
macro_rules! write_messages_match {
    ($writer:ident, $message:ident, $($discriminant:ident), *) => {
        match $message {
            $(NetMessage::$discriminant(message) => {
                $writer.write_out::<{ NETMSG_TYPE_BITS }, _>(message.get_type_())?;
                message.write($writer)?;
            },)*
        }
    };
}

/// Read all remaining messages from a bitstream. Any messages that are read
/// will be appended to `messages`.
pub fn read_messages(
    reader: &mut impl BitRead,
    side: MessageSide,
    messages: &mut Vec<NetMessage>,
) -> Result<(), NetMessageError> {
    loop {
        // I'm not entirely sure if this is correct, since it *might* be
        // possible for padding to be exactly 6 bits.
        match reader.read_in::<{ NETMSG_TYPE_BITS }, u8>() {
            Ok(message_type) => {
                let message = read_messages_match!(reader, side, message_type,
                    Nop         => Nop,
                    Disconnect  => Disconnect,
                    StringCmd   => StringCmd,
                    SetConVars  => SetConVars,
                    SignonState => SignonState,
                    Print       => Print,
                    File        => File,
                    ServerInfo  => ServerInfo,
                    Tick        => Tick);

                messages.push(message);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

/// Write all messages in `messages` to `writer`.
pub fn write_messages(
    writer: &mut impl BitWrite,
    messages: &[NetMessage],
) -> Result<(), NetMessageError> {
    for message in messages {
        write_messages_match!(
            writer,
            message,
            Nop,
            Disconnect,
            StringCmd,
            SetConVars,
            SignonState,
            Print,
            File,
            ServerInfo,
            Tick
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use bitstream_io::{BitReader, BitWriter, LittleEndian};

    use super::*;

    #[test]
    fn test_read_messages_single() {
        let mut writer = BitWriter::endian(Cursor::new(vec![]), LittleEndian);
        let expected_message = &[NetMessage::Print(Print {
            text: "test string yay".to_string(),
        })];

        write_messages(&mut writer, expected_message).unwrap();

        writer.byte_align().unwrap();

        let mut reader =
            BitReader::endian(Cursor::new(writer.into_writer().into_inner()), LittleEndian);
        let mut messages = vec![];
        read_messages(&mut reader, MessageSide::Client, &mut messages).unwrap();

        assert_eq!(messages, expected_message);
    }

    #[test]
    fn test_messages_roundtrip() {
        let mut writer = BitWriter::endian(Cursor::new(vec![]), LittleEndian);
        let expected_messages = &[
            NetMessage::Print(crate::net::netmessage::Print {
                text: "test string yay".to_string(),
            }),
            NetMessage::Disconnect(crate::net::netmessage::Disconnect {
                reason: "disconnect message (sad)".to_string(),
            }),
        ];

        write_messages(&mut writer, expected_messages).unwrap();

        writer.byte_align().unwrap();

        let mut reader =
            BitReader::endian(Cursor::new(writer.into_writer().into_inner()), LittleEndian);
        let mut messages = vec![];
        read_messages(&mut reader, MessageSide::Client, &mut messages).unwrap();

        assert_eq!(messages, expected_messages);
    }
}
