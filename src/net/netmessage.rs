use bitstream_io::{BitRead, BitWrite};

use crate::io_util::{read_string, write_string};

use super::message::{Message, MessageSide};

pub const NETMSG_TYPE_BITS: u32 = 6; // must be 2^NETMSG_TYPE_BITS > SVC_LASTMSG

#[derive(Debug, PartialEq, Eq)]
pub struct Nop;

impl Message<std::io::Error> for Nop {
    const TYPE: u8 = 0;
    const SIDE: MessageSide = MessageSide::Any;

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
pub struct Disconnect {
    pub reason: String,
}

impl Message<std::io::Error> for Disconnect {
    const TYPE: u8 = 1;
    const SIDE: MessageSide = MessageSide::Any;

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
pub struct StringCmd {
    pub command: String,
}

impl Message<std::io::Error> for StringCmd {
    const TYPE: u8 = 4;
    const SIDE: MessageSide = MessageSide::Any;

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
pub struct SetConVars {
    pub convars: Vec<(String, String)>,
}

impl Message<std::io::Error> for SetConVars {
    const TYPE: u8 = 5;
    const SIDE: MessageSide = MessageSide::Any;

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
pub struct SignonState {
    pub signon_state: u8,
    pub spawn_count: i32,
}

impl Message<std::io::Error> for SignonState {
    const TYPE: u8 = 6;
    const SIDE: MessageSide = MessageSide::Any;

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
pub struct Print {
    pub text: String,
}

impl Message<std::io::Error> for Print {
    const TYPE: u8 = 7;
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
pub enum FileMode {
    Request,
    Deny,
}
#[derive(Debug, PartialEq, Eq)]
pub struct File {
    pub mode: FileMode,
    pub filename: String,
    pub transfer_id: u32,
}

impl Message<std::io::Error> for File {
    const TYPE: u8 = 2;
    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized,
    {
        todo!()
    }

    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()> {
        writer.write_out::<32, u32>(self.transfer_id)?;
        write_string(writer, &self.filename)?;
        writer.write_bit(self.mode == FileMode::Request)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum NetMessage {
    Nop(Nop),
    Disconnect(Disconnect),
    StringCmd(StringCmd),
    SetConVars(SetConVars),
    SignonState(SignonState),
    Print(Print),
    File(File),
}

/// Helper to generate the match statement for reading messages
macro_rules! read_messages_match {
    ($reader:ident, $side:ident, $message_type:ident, $($struct:ident => $discriminant:ident), *) => {
        match $message_type {
            $($struct::TYPE if $struct::SIDE.can_receive($side) => NetMessage::$discriminant($struct::read($reader)?),)*
            // TODO: Use a custom error for this, instead of crashing.
            _ => todo!("unimplemented message type {} for side {:?}", $message_type, $side)
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
) -> Result<(), std::io::Error> {
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
                    File        => File);

                messages.push(message);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

/// Write all messages in `messages` to `writer`.
pub fn write_messages(writer: &mut impl BitWrite, messages: &[NetMessage]) -> std::io::Result<()> {
    for message in messages {
        write_messages_match!(writer, message,
            Nop,
            Disconnect,
            StringCmd,
            SetConVars,
            SignonState,
            Print,
            File);
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
