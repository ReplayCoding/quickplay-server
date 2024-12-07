use bitstream_io::{BitWrite, BitWriter as BackingBitWriter, LittleEndian};

type Cache = u64;
type HalfCache = u32;

pub trait Numeric: bitstream_io::Numeric {
    /// Convert a value read from the cache to Self
    fn from_half_cache(half_cache: HalfCache) -> Self;
}

macro_rules! impl_numeric {
    ($type:ty) => {
        impl Numeric for $type {
            fn from_half_cache(half_cache: HalfCache) -> Self {
                half_cache as $type
            }
        }
    };
}

impl_numeric!(u8);
impl_numeric!(u16);
impl_numeric!(u32);

pub struct BitReader<'a> {
    /// The data the bitstream will be read from
    data: &'a [u8],
    /// The position of the stream in bits
    position: usize,

    /// Holds any partially read data that hasn't been used
    cache: Cache,
    /// The number of bits currently available in the cache
    cache_bits: u8,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        // TODO: check that data is not too large
        Self {
            data,
            position: 0,
            cache: 0,
            cache_bits: 0,
        }
    }

    /// Refill the cache with as many bits as possible
    fn refill(&mut self) {
        const {
            assert!(
                size_of::<Cache>() / 2 == size_of::<HalfCache>(),
                "HalfCache must be exactly half of Cache"
            );
        }

        let byte_position = (self.position + usize::from(self.cache_bits)) / 8;

        let bytes_left = self.data.len() - byte_position;
        if usize::from(self.cache_bits) < (size_of::<HalfCache>() * 8) {
            if bytes_left >= size_of::<HalfCache>() {
                let bytes = &self.data[byte_position..byte_position + size_of::<HalfCache>()];
                let refill = HalfCache::from_le_bytes(bytes.try_into().unwrap());
                let filled_bits = (size_of_val(&refill) * 8) as u8;

                self.cache |= Cache::from(refill) << self.cache_bits;
                self.cache_bits += filled_bits;
            } else {
                let mut refill: HalfCache = 0;
                for i in 0..bytes_left {
                    refill |= HalfCache::from(self.data[byte_position + i]) << (i * 8);
                }
                let filled_bits = (bytes_left * 8) as u8;

                self.cache |= Cache::from(refill) << self.cache_bits;
                self.cache_bits += filled_bits;
            }
        }
    }

    /// Read out bits from the cache. If the cache does not have enough data,
    /// the data will be padded with zeroes
    fn drain_cache(&mut self, bits: u8) -> HalfCache {
        assert!(bits <= self.cache_bits);

        let mask = (1 << bits) - 1;
        let value = self.cache & mask;
        self.cache >>= bits;
        self.cache_bits -= bits;
        self.position += usize::from(bits);

        value as HalfCache
    }

    pub fn read_bit(&mut self) -> std::io::Result<bool> {
        // TODO: can this be optimized?
        Ok(self.read_in::<1, u8>()? != 0)
    }

    pub fn read_bytes(&mut self, data: &mut [u8]) -> std::io::Result<()> {
        // TODO: optimize
        for byte in data {
            *byte = self.read_in::<8, u8>()?;
        }

        Ok(())
    }

    pub fn read_in<const BITS: u8, Type: Numeric>(&mut self) -> std::io::Result<Type> {
        const {
            assert!(
                (BITS as usize) <= (size_of::<HalfCache>() * 8),
                "Cannot drain more than sizeof(HalfCache) bits"
            );
        }

        if BITS > self.cache_bits {
            self.refill();
            // Couldn't get any more :(
            if BITS > self.cache_bits {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
            }
        }

        Ok(Type::from_half_cache(self.drain_cache(BITS)))
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

    pub fn write_out<const BITS: u8, Type: Numeric>(&mut self, value: Type) -> std::io::Result<()> {
        self.writer.write::<Type>(BITS as u32, value)
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
        writer.write_bit(true).unwrap();
        writer.write_out::<3, u32>(0b011).unwrap();
        writer.write_out::<9, u32>(0b1_1001_1010).unwrap();

        let some_bytes_reference: [u8; 8] = [0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48];
        writer.write_bytes(&some_bytes_reference).unwrap();

        writer.write_out::<32, u32>(0x41_42_43_44).unwrap();
        writer.byte_align().unwrap();

        let data = writer.into_bytes();
        let mut reader = BitReader::new(&data);
        assert_eq!(0x12_34_56, reader.read_in::<24, u32>().unwrap());
        assert_eq!(true, reader.read_bit().unwrap());
        assert_eq!(0b011, reader.read_in::<3, u32>().unwrap());
        assert_eq!(0b1_1001_1010, reader.read_in::<9, u32>().unwrap());

        let mut some_bytes_actual = [0u8; 8];
        reader.read_bytes(&mut some_bytes_actual).unwrap();
        assert!(some_bytes_actual == some_bytes_reference);

        assert_eq!(0x41_42_43_44, reader.read_in::<32, u32>().unwrap());
    }

    #[test]
    fn test_reader_full_refill() {
        let bytes = [0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.cache, 0);
        assert_eq!(reader.cache_bits, 0);

        reader.refill();
        assert_eq!(reader.cache, 0x44_43_42_41);
        assert_eq!(reader.cache_bits, 32);

        // Repeated refills shouldn't try to load any more data
        reader.refill();
        assert_eq!(reader.cache, 0x44_43_42_41);
        assert_eq!(reader.cache_bits, 32);
    }

    #[test]
    fn test_reader_partial_refill() {
        let bytes = [0x41, 0x42, 0x43];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.cache, 0);
        assert_eq!(reader.cache_bits, 0);

        reader.refill();
        assert_eq!(reader.cache, 0x00_43_42_41);
        assert_eq!(reader.cache_bits, 24);

        // Repeated refills shouldn't try to load any more data
        reader.refill();
        assert_eq!(reader.cache, 0x00_43_42_41);
        assert_eq!(reader.cache_bits, 24);
    }

    #[test]
    fn test_reader_drain() {
        let bytes = [0x41, 0x42, 0x43, 0x44, 0x67];
        let mut reader = BitReader::new(&bytes);
        reader.refill();

        assert_eq!(reader.drain_cache(8), 0x41);
        assert_eq!(reader.cache, 0x44_43_42);
        assert_eq!(reader.cache_bits, 24);

        assert_eq!(reader.drain_cache(4), 0x2);
        assert_eq!(reader.cache, 0x04_44_34);
        assert_eq!(reader.cache_bits, 20);

        assert_eq!(reader.cache, 0b0100_0100_0100_0011_0100u64);
        assert_eq!(reader.drain_cache(5), 0b1_0100);
        assert_eq!(reader.cache, 0b0100_0100_0100_001u64);
        assert_eq!(reader.cache_bits, 15);

        assert_eq!(reader.drain_cache(7), 0b0100_001);
        assert_eq!(reader.cache, 0b0100_0100_u64);
        assert_eq!(reader.cache_bits, 8);

        assert_eq!(reader.drain_cache(4), 0x04);
        assert_eq!(reader.cache, 0x4);
        assert_eq!(reader.cache_bits, 4);

        reader.refill();

        assert_eq!(reader.drain_cache(8), 0x74);
        assert_eq!(reader.cache, 0x6);
        assert_eq!(reader.cache_bits, 4);
    }

    #[test]
    fn test_reader_overflow() {
        let bytes = [0x41, 0x42, 0x43, 0x44];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_in::<16, u16>().unwrap(), 0x42_41);
        // Reading too many bits should fail
        assert!(reader.read_in::<17, u32>().is_err());
        // If the reader returns an overflow error, it should move the position
        assert_eq!(reader.read_in::<16, u16>().unwrap(), 0x44_43);
        // Check exact edge cases around the end of the stream
        assert!(reader.read_in::<1, u8>().is_err());
    }
}
