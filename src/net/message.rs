use bitstream_io::{BitRead, BitWrite};

/// The sides that a message may be sent from
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MessageSide {
    Client,
    Server,

    Any,
}

impl MessageSide {
    pub fn can_receive(&self, other: Self) -> bool {
        if *self == MessageSide::Any {
            return true;
        }

        // Client can receive messages from Server
        *self != other
    }
}

/// A single message.
pub(super) trait Message {
    /// This is used to identify the message. It may collide with other
    /// messages, as long as those messages cannot be receieved from the same
    /// side.
    const TYPE: u8;
    const SIDE: MessageSide;

    /// Hack to allow macros to access the message type
    fn get_type_(&self) -> u8 {
        Self::TYPE
    }

    fn read(reader: &mut impl BitRead) -> std::io::Result<Self>
    where
        Self: Sized;
    fn write(&self, writer: &mut impl BitWrite) -> std::io::Result<()>;
}
