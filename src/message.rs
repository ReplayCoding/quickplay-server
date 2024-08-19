use anyhow::anyhow;
use bitstream_io::{BitRead, BitWrite};

use crate::{
    io_util::{read_string, write_string},
    netchannel::NETMSG_TYPE_BITS,
};

const MESSAGE_TYPE_DISCONNECT: u32 = 1; // disconnect, last message in connection
const MESSAGE_TYPE_STRINGCMD: u32 = 4; // a string command
const MESSAGE_TYPE_SET_CON_VAR: u32 = 5; // sends one/multiple convar settings
const MESSAGE_TYPE_SIGNON_STATE: u32 = 6; // signals current signon state
const MESSAGE_TYPE_PRINT: u32 = 7; // print text to console

#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Disconnect(MessageDisconnect),
    StringCmd(MessageStringCmd),
    SetConVars(MessageSetConVars),
    SignonState(MessageSignonState),
    Print(MessagePrint),
}

impl Message {
    pub fn read<R: BitRead>(reader: &mut R, message_type: u32) -> anyhow::Result<Self> {
        match message_type {
            MESSAGE_TYPE_DISCONNECT => {
                let reason = read_string(reader, 1024)?;
                Ok(Message::Disconnect(MessageDisconnect { reason }))
            }
            MESSAGE_TYPE_SET_CON_VAR => {
                let num_vars: u8 = reader.read_in::<8, _>()?;
                let mut convars = vec![];
                convars.reserve(num_vars.into());

                for _ in 0..num_vars {
                    let name = read_string(reader, 260)?;
                    let value = read_string(reader, 260)?;
                    convars.push((name, value));
                }

                Ok(Message::SetConVars(MessageSetConVars { convars }))
            }
            MESSAGE_TYPE_SIGNON_STATE => {
                let signon_state: u8 = reader.read_in::<8, _>()?;
                let spawn_count: i32 = reader.read_in::<32, _>()?;

                Ok(Message::SignonState(MessageSignonState {
                    signon_state,
                    spawn_count,
                }))
            }
            MESSAGE_TYPE_PRINT => Ok(Message::Print(MessagePrint {
                text: read_string(reader, 2048)?,
            })),
            _ => Err(anyhow!("unhandled message type {message_type}")),
        }
    }

    pub fn write<W: BitWrite>(&self, writer: &mut W) -> anyhow::Result<()> {
        match self {
            Message::StringCmd(message) => {
                writer.write_out::<NETMSG_TYPE_BITS, _>(MESSAGE_TYPE_STRINGCMD)?;
                write_string(writer, &message.command)?;
            }
            Message::Print(message) => {
                writer.write_out::<NETMSG_TYPE_BITS, _>(MESSAGE_TYPE_PRINT)?;
                write_string(writer, &message.text)?;
            }
            Message::Disconnect(message) => {
                writer.write_out::<NETMSG_TYPE_BITS, _>(MESSAGE_TYPE_DISCONNECT)?;
                write_string(writer, &message.reason)?;
            }

            _ => return Err(anyhow!("unhandled message {:?} for write", self)),
        };

        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageDisconnect {
    pub reason: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageStringCmd {
    pub command: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageSetConVars {
    pub convars: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageSignonState {
    pub signon_state: u8,
    pub spawn_count: i32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessagePrint {
    pub text: String,
}
