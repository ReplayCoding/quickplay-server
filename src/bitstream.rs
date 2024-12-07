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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let mut writer = BitWriter::new();

        writer.write_out::<24, u32>(0x12_34_56).unwrap();
        writer.write_out::<3, u32>(0b011).unwrap();
        writer.write_out::<9, u32>(0b1_1001_1010).unwrap();
        writer.write_out::<32, u32>(0x41_42_43_44).unwrap();
        writer.byte_align().unwrap();

        let data = writer.into_bytes();
        let mut reader = BitReader::new(&data);
        assert_eq!(0x12_34_56, reader.read_in::<24, u32>().unwrap());
        assert_eq!(0b011, reader.read_in::<3, u32>().unwrap());
        assert_eq!(0b1_1001_1010, reader.read_in::<9, u32>().unwrap());
        assert_eq!(0x41_42_43_44, reader.read_in::<32, u32>().unwrap());
    }
}
