use bitstream_io::{BitRead, BitWrite};

use crate::io_util::{read_string, write_string};

pub const NETMSG_TYPE_BITS: u32 = 6; // must be 2^NETMSG_TYPE_BITS > SVC_LASTMSG

/// The sides that a message may be sent from
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MessageSide {
    Client,
    Server,

    /// DO NOT USE OUTSIDE OF MessageTrait.
    Both,
}

impl MessageSide {
    fn can_receive(&self, other: Self) -> bool {
        if *self == MessageSide::Both {
            return true;
        }

        // Client can receive messages from Server
        *self != other
    }
}

/// A single netmessage. This roughly corresponds to CNetMessage in the official
/// implementation. (terrible name i know)
trait MessageTrait {
    /// This is the message type, sent before the actual message is written. It
    /// may collide with other messages, as long as those messages do not share
    /// the same side (or Both). The type must fit into NETMSG_TYPE_BITS bits.
    const TYPE: u32;
    const SIDE: MessageSide;

    /// Hack to allow macros to access the message type
    fn get_type(&self) -> u32 {
        Self::TYPE
    }

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized;
    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()>;
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageNop;

impl MessageTrait for MessageNop {
    const TYPE: u32 = 0;
    const SIDE: MessageSide = MessageSide::Both;

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized,
    {
        Ok(Self)
    }

    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageDisconnect {
    pub reason: String,
}

impl MessageTrait for MessageDisconnect {
    const TYPE: u32 = 1;
    const SIDE: MessageSide = MessageSide::Both;

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized,
    {
        let reason = read_string(reader, 1024)?;
        Ok(Self { reason })
    }

    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()>
    where
        Self: Sized,
    {
        write_string(writer, &self.reason)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageStringCmd {
    pub command: String,
}

impl MessageTrait for MessageStringCmd {
    const TYPE: u32 = 4;
    const SIDE: MessageSide = MessageSide::Both;

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized,
    {
        let command = read_string(reader, 1024)?;
        Ok(Self { command })
    }

    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()> {
        write_string(writer, &self.command)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageSetConVars {
    pub convars: Vec<(String, String)>,
}

impl MessageTrait for MessageSetConVars {
    const TYPE: u32 = 5;
    const SIDE: MessageSide = MessageSide::Both;

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized,
    {
        let num_convars = reader.read_in::<8, u8>()?;
        let mut convars = vec![];
        for _ in 0..num_convars {
            let name = read_string(reader, 260)?;
            let value = read_string(reader, 260)?;
            convars.push((name, value));
        }
        Ok(Self { convars })
    }

    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()> {
        todo!()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageSignonState {
    pub signon_state: u8,
    pub spawn_count: i32,
}

impl MessageTrait for MessageSignonState {
    const TYPE: u32 = 6;
    const SIDE: MessageSide = MessageSide::Both;

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
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

    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()> {
        todo!()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessagePrint {
    pub text: String,
}

impl MessageTrait for MessagePrint {
    const TYPE: u32 = 7;
    const SIDE: MessageSide = MessageSide::Server;

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized,
    {
        let text = read_string(reader, 2048)?;
        Ok(Self { text })
    }

    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()> {
        write_string(writer, &self.text)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MessageFileMode {
    Request,
    Deny,
}
#[derive(Debug, PartialEq, Eq)]
pub struct MessageFile {
    pub mode: MessageFileMode,
    pub filename: String,
    pub transfer_id: u32,
}

impl MessageTrait for MessageFile {
    const TYPE: u32 = 2;
    const SIDE: MessageSide = MessageSide::Both;

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized,
    {
        todo!()
    }

    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()> {
        writer.write_out::<32, u32>(self.transfer_id)?;
        write_string(writer, &self.filename)?;
        writer.write_bit(self.mode == MessageFileMode::Request)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Nop(MessageNop),
    Disconnect(MessageDisconnect),
    StringCmd(MessageStringCmd),
    SetConVars(MessageSetConVars),
    SignonState(MessageSignonState),
    Print(MessagePrint),
    File(MessageFile),
}

/// Helper to generate the match statement for reading messages
macro_rules! read_messages_match {
    ($reader:ident, $side:ident, $message_type:ident, $($struct:ident => $discriminant:ident), *) => {
        match $message_type {
            $($struct::TYPE if $struct::SIDE.can_receive($side) => crate::net::message::Message::$discriminant($struct::read($reader)?),)*
            _ => todo!("unimplemented message type {} for side {:?}", $message_type, $side)
        }
    };
}

/// Helper to generate the match statement for writing messages
macro_rules! write_messages_match {
    ($writer:ident, $message:ident, $($struct:ident => $discriminant:ident), *) => {
        match $message {
            $(crate::net::message::Message::$discriminant(message) => {
                $writer.write_out::<{ NETMSG_TYPE_BITS }, u32>(message.get_type())?;
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
    messages: &mut Vec<Message>,
) -> Result<(), std::io::Error> {
    loop {
        // I'm not entirely sure if this is correct, since it *might* be
        // possible for padding to be exactly 6 bits.
        match reader.read_in::<{ NETMSG_TYPE_BITS }, u32>() {
            Ok(message_type) => {
                let message = read_messages_match!(reader, side, message_type,
                    MessageNop         => Nop,
                    MessageDisconnect  => Disconnect,
                    MessageStringCmd   => StringCmd,
                    MessageSetConVars  => SetConVars,
                    MessageSignonState => SignonState,
                    MessagePrint       => Print,
                    MessageFile        => File);

                messages.push(message);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

/// Write all messages in `messages` to `writer`.
pub fn write_messages(writer: &mut impl BitWrite, messages: &[Message]) -> std::io::Result<()> {
    for message in messages {
        write_messages_match!(writer, message,
            MessageNop         => Nop,
            MessageDisconnect  => Disconnect,
            MessageStringCmd   => StringCmd,
            MessageSetConVars  => SetConVars,
            MessageSignonState => SignonState,
            MessagePrint       => Print,
            MessageFile        => File);
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
        let expected_message = &[Message::Print(MessagePrint {
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
            Message::Print(crate::net::message::MessagePrint {
                text: "test string yay".to_string(),
            }),
            Message::Disconnect(crate::net::message::MessageDisconnect {
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
