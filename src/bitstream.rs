use bitstream_io::{
    BitRead, BitReader as BackingBitReader, BitWrite, BitWriter as BackingBitWriter, LittleEndian,
    Numeric,
};

pub struct BitReader<'a> {
    reader: BackingBitReader<&'a [u8], LittleEndian>,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            reader: BackingBitReader::endian(data, LittleEndian),
        }
    }

    pub fn read_bit(&mut self) -> std::io::Result<bool> {
        self.reader.read_bit()
    }

    pub fn read_bytes(&mut self, data: &mut [u8]) -> std::io::Result<()> {
        self.reader.read_bytes(data)
    }

    pub fn read_in<const BITS: u32, Type: Numeric>(&mut self) -> std::io::Result<Type> {
        self.reader.read_in::<BITS, Type>()
    }
}

pub struct BitWriter {
    writer: BackingBitWriter<Vec<u8>, LittleEndian>,
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            writer: BackingBitWriter::endian(vec![], LittleEndian),
        }
    }

    pub fn write_out<const BITS: u32, Type: Numeric>(
        &mut self,
        value: Type,
    ) -> std::io::Result<()> {
        self.writer.write_out::<BITS, Type>(value)
    }

    pub fn write_bit(&mut self, value: bool) -> std::io::Result<()> {
        self.writer.write_bit(value)
    }

    pub fn write_bytes(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_bytes(data)
    }

    pub fn byte_align(&mut self) -> std::io::Result<()> {
        self.writer.byte_align()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.writer.into_writer()
    }
}
