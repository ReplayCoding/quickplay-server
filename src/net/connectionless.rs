// Connectionless message types are named such that this will cause a bunch of
// warnings.
#![allow(non_camel_case_types)]
use std::io::Cursor;

use bitstream_io::{BitRead, BitReader, BitWrite, BitWriter, LittleEndian};
use strum::EnumDiscriminants;
use thiserror::Error;

use crate::io_util::{read_string, write_string};

use super::{
    message::{Message, MessageSide},
    packet::CONNECTIONLESS_HEADER,
};

#[derive(Error, Debug)]
pub enum ConnectionlessError {
    #[error("invalid auth protocol: {0}")]
    InvalidAuthProtocol(u32),
    #[error("steam auth key is too long: {0}")]
    SteamAuthKeyTooLong(usize),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug)]
pub enum ConnectionlessMessage {
    A2S_GetChallenge(A2S_GetChallenge),
    C2S_Connect(C2S_Connect),
    S2C_Challenge(S2C_Challenge),
    S2C_Connection(S2C_Connection),
    S2C_ConnReject(S2C_ConnReject),
}

#[derive(Debug)]
pub struct A2S_GetChallenge {
    challenge: u32,
}

impl Message<ConnectionlessError> for A2S_GetChallenge {
    const TYPE: u8 = b'q';

    const SIDE: MessageSide = MessageSide::Any;

    fn read(reader: &mut impl BitRead) -> Result<Self, ConnectionlessError>
    where
        Self: Sized,
    {
        let challenge = reader.read_in::<32, u32>()?;
        Ok(Self { challenge })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), ConnectionlessError> {
        todo!()
    }
}

#[derive(Debug, EnumDiscriminants)]
#[strum_discriminants(name(AuthProtocolType))]
enum AuthProtocol {
    AuthCertificate(String),
    HashedCdKey(String),
    Steam(Vec<u8>),
}

#[derive(Debug)]
pub struct C2S_Connect {
    protocol_version: u32,
    auth_protocol: AuthProtocol,
    server_challenge: u32,
    client_challenge: u32,
    name: String,
    password: String,
    product_version: String,
}

impl Message<ConnectionlessError> for C2S_Connect {
    const TYPE: u8 = b'k';

    const SIDE: MessageSide = MessageSide::Client;

    fn read(reader: &mut impl BitRead) -> Result<Self, ConnectionlessError>
    where
        Self: Sized,
    {
        let protocol_version: u32 = reader.read_in::<32, _>()?;
        let auth_protocol: u32 = reader.read_in::<32, _>()?;
        let server_challenge: u32 = reader.read_in::<32, _>()?;
        let client_challenge: u32 = reader.read_in::<32, _>()?;
        let name = read_string(reader, 256)?;
        let password = read_string(reader, 256)?;
        let product_version = read_string(reader, 32)?;

        let auth_protocol = match auth_protocol {
            1 => AuthProtocol::AuthCertificate(read_string(reader, 2048)?),
            2 => AuthProtocol::HashedCdKey(read_string(reader, 2048)?),
            3 => {
                let key_len = reader.read_in::<16, u16>()?;
                if key_len > 2048 {
                    return Err(ConnectionlessError::SteamAuthKeyTooLong(key_len.into()));
                }

                let mut key = vec![0u8; usize::from(key_len)];
                reader.read_bytes(&mut key)?;

                AuthProtocol::Steam(key)
            }
            _ => return Err(ConnectionlessError::InvalidAuthProtocol(auth_protocol)),
        };

        Ok(Self {
            protocol_version,
            auth_protocol,
            server_challenge,
            client_challenge,
            name,
            password,
            product_version,
        })
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), ConnectionlessError> {
        todo!()
    }
}

#[derive(Debug)]
pub struct S2C_Challenge {
    server_challenge: u32,
    client_challenge: u32,
    auth_protocol: AuthProtocolType,
}

impl Message<ConnectionlessError> for S2C_Challenge {
    const TYPE: u8 = b'A';

    const SIDE: MessageSide = MessageSide::Server;

    fn read(reader: &mut impl BitRead) -> Result<Self, ConnectionlessError>
    where
        Self: Sized,
    {
        todo!()
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), ConnectionlessError> {
        writer.write_out::<32, _>(0x5a4f_4933)?; // S2C_MAGICVERSION
        writer.write_out::<32, _>(self.server_challenge)?;
        writer.write_out::<32, _>(self.client_challenge)?;

        writer.write_out::<32, _>(match self.auth_protocol {
            AuthProtocolType::AuthCertificate => 1,
            AuthProtocolType::HashedCdKey => 2,
            AuthProtocolType::Steam => 3,
        })?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct S2C_Connection {
    client_challenge: u32,
}

impl Message<ConnectionlessError> for S2C_Connection {
    const TYPE: u8 = b'B';

    const SIDE: MessageSide = MessageSide::Server;

    fn read(reader: &mut impl BitRead) -> Result<Self, ConnectionlessError>
    where
        Self: Sized,
    {
        todo!()
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), ConnectionlessError> {
        writer.write_out::<32, _>(self.client_challenge)?;
        write_string(writer, "0000000000")?; // padding

        Ok(())
    }
}

#[derive(Debug)]
pub struct S2C_ConnReject {
    client_challenge: u32,
    message: String,
}

impl Message<ConnectionlessError> for S2C_ConnReject {
    const TYPE: u8 = b'9';

    const SIDE: MessageSide = MessageSide::Server;

    fn read(reader: &mut impl BitRead) -> Result<Self, ConnectionlessError>
    where
        Self: Sized,
    {
        todo!()
    }

    fn write(&self, writer: &mut impl BitWrite) -> Result<(), ConnectionlessError> {
        writer.write_out::<32, _>(self.client_challenge)?;
        write_string(writer, &self.message)?;

        Ok(())
    }
}

/// Helper to generate the match statement for reading messages
macro_rules! read_message_match {
    ($reader:ident, $side:ident, $message_type:ident, $($struct:ident => $discriminant:ident), *) => {
        match $message_type {
            $($struct::TYPE if $struct::SIDE.can_receive($side) => ConnectionlessMessage::$discriminant($struct::read(&mut $reader)?),)*
            // TODO: Use a custom error for this, instead of crashing.
            _ => todo!("unimplemented message type {} for side {:?}", $message_type as char, $side)
        }
    };
}

/// Helper to generate the match statement for writing messages
macro_rules! write_message_match {
    ($writer:ident, $message:ident, $($discriminant:ident), *) => {
        match $message {
            $(ConnectionlessMessage::$discriminant(message) => {
                $writer.write_out::<8, _>(message.get_type_())?;
                message.write(&mut $writer)?
            },)*
        }
    };
}

impl ConnectionlessMessage {
    /// Read a single connectionless message.
    pub fn read(data: &[u8], side: MessageSide) -> Result<Self, ConnectionlessError> {
        let mut reader = BitReader::endian(Cursor::new(data), LittleEndian);
        _ = reader.read_in::<32, u32>()?; // consume 0xFFFF_FFFF connectionless header

        let message_type = reader.read_in::<8, u8>()?;
        Ok(read_message_match!(reader, side, message_type,
            A2S_GetChallenge => A2S_GetChallenge,
            C2S_Connect      => C2S_Connect,
            S2C_Challenge    => S2C_Challenge,
            S2C_Connection   => S2C_Connection,
            S2C_ConnReject   => S2C_ConnReject
        ))
    }

    pub fn write(&self) -> Result<Vec<u8>, ConnectionlessError> {
        let mut writer = BitWriter::endian(Cursor::new(Vec::<u8>::new()), LittleEndian);
        writer.write_out::<32, _>(CONNECTIONLESS_HEADER)?;
        write_message_match!(
            writer,
            self,
            A2S_GetChallenge,
            C2S_Connect,
            S2C_Challenge,
            S2C_Connection,
            S2C_ConnReject
        );

        Ok(writer.into_writer().into_inner())
    }
}

pub mod server_machine {
    use std::{
        hash::{DefaultHasher, Hash, Hasher},
        net::SocketAddr,
    };

    use tracing::trace;

    use crate::net::{connectionless::ConnectionlessMessage, message::MessageSide};

    use super::{
        AuthProtocolType, ConnectionlessError, S2C_Challenge, S2C_ConnReject, S2C_Connection,
    };

    const PROTOCOL_VERSION: u8 = 24;

    /// Create an opaque challenge number for an address, which will be consistent for this address
    fn get_challenge_for_address(addr: SocketAddr) -> u32 {
        // TODO: evaluate if this hasher fits our needs
        let mut hasher = DefaultHasher::new();
        addr.hash(&mut hasher);

        // intentionally truncating the value
        hasher.finish() as u32
    }

    pub struct ConnectionlessServerMachineResponse {
        /// The response to send back to the client
        pub response: Option<Vec<u8>>,
        /// If this is Some, a connection should be established using this
        /// challenge value.
        pub challenge: Option<u32>,
    }

    /// Run the connectionless server state machine. `data` is the received
    /// packet data, including the header.
    pub fn connectionless_server_machine(
        data: &[u8],
        from: SocketAddr,
    ) -> Result<ConnectionlessServerMachineResponse, ConnectionlessError> {
        let message = ConnectionlessMessage::read(data, MessageSide::Server)?;
        match message {
            ConnectionlessMessage::A2S_GetChallenge(message) => {
                let server_challenge = get_challenge_for_address(from);
                Ok(ConnectionlessServerMachineResponse {
                    response: Some(
                        ConnectionlessMessage::S2C_Challenge(S2C_Challenge {
                            server_challenge,
                            client_challenge: message.challenge,
                            auth_protocol: AuthProtocolType::HashedCdKey,
                        })
                        .write()?,
                    ),
                    challenge: None,
                })
            }

            ConnectionlessMessage::C2S_Connect(message) => {
                if message.server_challenge != get_challenge_for_address(from) {
                    return reject_connection(
                        message.client_challenge,
                        "#GameUI_ServerRejectBadChallenge",
                    );
                }

                // TODO: check protocol version
                // TODO: check auth protocol

                Ok(ConnectionlessServerMachineResponse {
                    response: Some(
                        ConnectionlessMessage::S2C_Connection(S2C_Connection {
                            client_challenge: message.client_challenge,
                        })
                        .write()?,
                    ),
                    challenge: Some(message.server_challenge),
                })
            }

            _ => {
                trace!("unhandled message received: {:?}", message);

                // TODO: maybe make this an error instead?
                Ok(ConnectionlessServerMachineResponse {
                    response: None,
                    challenge: None,
                })
            }
        }
    }

    fn reject_connection(
        client_challenge: u32,
        message: &str,
    ) -> Result<ConnectionlessServerMachineResponse, ConnectionlessError> {
        Ok(ConnectionlessServerMachineResponse {
            response: Some(
                ConnectionlessMessage::S2C_ConnReject(S2C_ConnReject {
                    client_challenge,
                    message: message.to_string(),
                })
                .write()?,
            ),
            challenge: None,
        })
    }
}
