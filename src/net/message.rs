use crate::io::bitstream::{BitReader, BitWriter};

/// The sides that a message may be sent from
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MessageSide {
    Client,
    Server,

    Any,
}

impl MessageSide {
    pub fn can_receive(&self, receiving_side: Self) -> bool {
        if *self == MessageSide::Any {
            return true;
        }

        // a client can receive messages from the server, but not from a client
        *self != receiving_side
    }
}

/// A single generic message.
pub(super) trait Message<Err> {
    /// This is used to identify the message. It may collide with other
    /// messages, as long as those messages cannot be receieved from the same
    /// side.
    const TYPE: u8;
    /// The side of the connection that this message can be **sent** from
    const SIDE: MessageSide;

    /// Hack to allow macros to access the message type
    fn get_type_(&self) -> u8 {
        Self::TYPE
    }

    fn read(reader: &mut BitReader) -> Result<Self, Err>
    where
        Self: Sized;
    fn write(&self, writer: &mut BitWriter) -> Result<(), Err>;
}
