use bitstream_io::{BitRead, BitWrite};

use crate::io_util::{read_string, write_string};

pub const NETMSG_TYPE_BITS: u32 = 6; // must be 2^NETMSG_TYPE_BITS > SVC_LASTMSG

const MESSAGE_TYPE_DISCONNECT: u32 = 1; // disconnect, last message in connection
const MESSAGE_TYPE_FILE: u32 = 2; // file request or denial
const MESSAGE_TYPE_STRINGCMD: u32 = 4; // a string command
const MESSAGE_TYPE_SET_CON_VAR: u32 = 5; // sends one/multiple convar settings
const MESSAGE_TYPE_SIGNON_STATE: u32 = 6; // signals current signon state
const MESSAGE_TYPE_PRINT: u32 = 7; // print text to console

#[derive(Debug)]
pub enum Message {
    Disconnect(MessageDisconnect),
    File(MessageFile),
    StringCmd(MessageStringCmd),
    SetConVars(MessageSetConVars),
    SignonState(MessageSignonState),
    Print(MessagePrint),
}

impl Message {
    pub fn read<R: BitRead>(reader: &mut R, message_type: u32) -> std::io::Result<Self> {
        match message_type {
            MESSAGE_TYPE_DISCONNECT => {
                let reason = read_string(reader, 1024)?;
                Ok(Message::Disconnect(MessageDisconnect { reason }))
            }
            MESSAGE_TYPE_SET_CON_VAR => {
                let num_vars: u8 = reader.read_in::<8, _>()?;
                let mut convars = Vec::with_capacity(num_vars.into());

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

            MESSAGE_TYPE_FILE => {
                let transfer_id = reader.read_in::<32, u32>()?;
                let filename = read_string(reader, 1024)?;
                let mode = if reader.read_bit()? {
                    MessageFileMode::Request
                } else {
                    MessageFileMode::Deny
                };
                Ok(Message::File(MessageFile {
                    mode,
                    filename,
                    transfer_id,
                }))
            }

            _ => todo!(),
        }
    }

    pub fn write<W: BitWrite>(&self, writer: &mut W) -> std::io::Result<()> {
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
            Message::File(message) => {
                writer.write_out::<NETMSG_TYPE_BITS, _>(MESSAGE_TYPE_FILE)?;
                writer.write_out::<32, _>(message.transfer_id)?;
                write_string(writer, &message.filename)?;
                writer.write_bit(message.mode == MessageFileMode::Request)?;
            }

            _ => todo!(),
        };

        Ok(())
    }
}

#[derive(Debug)]
pub struct MessageDisconnect {
    pub reason: String,
}

#[derive(Debug)]
pub struct MessageStringCmd {
    pub command: String,
}

#[derive(Debug)]
pub struct MessageSetConVars {
    pub convars: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct MessageSignonState {
    pub signon_state: u8,
    pub spawn_count: i32,
}

#[derive(Debug)]
pub struct MessagePrint {
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub enum MessageFileMode {
    Request,
    Deny,
}
#[derive(Debug)]
pub struct MessageFile {
    pub mode: MessageFileMode,
    pub filename: String,
    pub transfer_id: u32,
}
