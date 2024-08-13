use anyhow::anyhow;
use bitstream_io::BitRead;

use crate::io_util::read_string;

const MESSAGE_TYPE_DISCONNECT: u32 = 1; // disconnect, last message in connection
const MESSAGE_TYPE_SET_CON_VAR: u32 = 5; // sends one/multiple convar settings
const MESSAGE_TYPE_SIGNON_STATE: u32 = 6; // signals current signon state

#[derive(Debug)]
pub enum Message {
    Disconnect(MessageDisconnect),
    SetConVars(MessageSetConVars),
    SignonState(MessageSignonState),
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
            _ => Err(anyhow!("unknown message type {message_type}")),
        }
    }
}

#[derive(Debug)]
pub struct MessageDisconnect {
    pub reason: String,
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
