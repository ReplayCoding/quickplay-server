use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    sync::{
        mpsc::{Receiver, Sender},
        Arc, RwLock,
    },
};

use tracing::{debug, error, trace};

use crate::net::{
    connectionless::server_machine::{
        connectionless_server_machine, ConnectionlessServerMachineResponse,
    },
    packet::{decode_raw_packet, Packet},
};

/// Represents a single connection
pub trait Connection: Send {
    /// This will be called in a background thread dedicated to this connection.
    /// The connection will be terminated when this returns.
    fn run(&mut self);
}

pub struct ConnectionSetupInfo {
    /// The socket to send data back with
    pub socket: Arc<UdpSocket>,
    /// The address of the client for this connection
    pub client_addr: SocketAddr,
    /// Receiver for incoming packets. If the channel is shutdown, then the
    /// `run` method of your connection should immediately terminate the
    /// connection by returning.
    pub incoming_packets: Receiver<Vec<u8>>,
    /// The challenge number to use
    pub challenge: u32,
}

type ConnectionFactory = Box<dyn FnMut(ConnectionSetupInfo) -> Box<dyn Connection>>;

struct ConnectionWrapper {
    packet_sender: Sender<Vec<u8>>,
    /// This cookie is intended to uniquely identify the connection, even if it
    /// has the same slot in the connection map. Theoretically, a client could
    /// overflow the global cookie and create a new connection with the same
    /// cookie as an existing connection, but in practice this doesn't matter.
    /// Ultimately this doesn't matter since the worst thing that can happen is
    /// that the new connection will be immediately dropped.
    cookie: u64,
}

pub struct Server {
    socket: Arc<UdpSocket>,
    connections: Arc<RwLock<HashMap<SocketAddr, ConnectionWrapper>>>,
    connection_factory: ConnectionFactory,
    cookie: u64,
}

impl Server {
    /// Create a new server running at the specified address.
    pub fn new(addr: SocketAddr, connection_factory: ConnectionFactory) -> std::io::Result<Self> {
        Ok(Self {
            socket: UdpSocket::bind(addr)?.into(),
            connections: Arc::new(HashMap::new().into()),
            connection_factory,
            cookie: 0,
        })
    }

    /// Run the server.
    pub fn run(&mut self) {
        let mut packet_buffer = vec![0u8; u16::MAX.into()];
        loop {
            match self.socket.recv_from(&mut packet_buffer) {
                Ok((size, from)) => {
                    let packet_buffer = &packet_buffer[..size];
                    if let Err(err) = self.handle_packet(packet_buffer, from) {
                        debug!("error while handling packet: {err}");
                    }
                }
                Err(err) => error!("error while reading packet from socket: {err}"),
            }
        }
    }

    fn handle_packet(&mut self, packet: &[u8], from: SocketAddr) -> anyhow::Result<()> {
        match decode_raw_packet(packet)? {
            Packet::Connectionless(packet) => self.handle_packet_connectionless(&packet, from)?,
            Packet::Regular(packet) => {
                self.handle_packet_with_connection(from, packet.into_owned())?;
            }
        };

        Ok(())
    }

    fn handle_packet_connectionless(
        &mut self,
        packet: &[u8],
        from: SocketAddr,
    ) -> anyhow::Result<()> {
        let ConnectionlessServerMachineResponse {
            response,
            challenge,
        } = connectionless_server_machine(packet, from)?;

        if let Some(response) = response {
            self.socket.send_to(&response, from)?;
        }

        if let Some(challenge) = challenge {
            self.create_connection(challenge, from)?;
        }

        Ok(())
    }

    fn handle_packet_with_connection(
        &mut self,
        from: SocketAddr,
        packet: Vec<u8>,
    ) -> anyhow::Result<()> {
        let connections = self.connections.read().unwrap();
        if let Some(connection) = connections.get(&from) {
            connection.packet_sender.send(packet)?
        };

        Ok(())
    }

    fn create_connection(&mut self, challenge: u32, from: SocketAddr) -> anyhow::Result<()> {
        trace!("creating new connection for {from} with challenge {challenge:04x}");
        let (sender, receiver) = std::sync::mpsc::channel();
        let setup_info = ConnectionSetupInfo {
            socket: self.socket.clone(),
            client_addr: from,
            challenge,
            incoming_packets: receiver,
        };

        let cookie = self.cookie;
        let connection_wrapper = ConnectionWrapper {
            packet_sender: sender,
            cookie,
        };

        let mut connection = (self.connection_factory)(setup_info);
        // Make a copy of connections for the new thread
        let connections = self.connections.clone();
        std::thread::spawn(move || {
            connection.run();
            let mut connections = connections.write().unwrap();
            let Some(connection_in_map) = connections.get(&from) else {
                // If this is reached, something has gone catastrophically wrong.
                error!("trying to destroy connection for {from}, but no connection exists!");
                return;
            };

            if connection_in_map.cookie == cookie {
                connections.remove(&from);
                trace!("destroyed connection for {from}");
            } else {
                // If the cookie doesn't match, then this is a different connection,
                // so don't remove it.
                trace!(
                    "connection cookie for {from} didn't match with stored cookie, not destroying"
                )
            }
        });

        self.cookie += 1;
        self.connections
            .write()
            .unwrap()
            .insert(from, connection_wrapper);

        Ok(())
    }
}
