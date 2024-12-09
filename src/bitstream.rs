type Cache = u64;
type HalfCache = u32;
const _: () = assert!(
    size_of::<Cache>() / 2 == size_of::<HalfCache>(),
    "HalfCache must be exactly half of Cache"
);

pub trait Numeric {
    /// Convert a value read from the cache to Self
    fn from_half_cache(half_cache: HalfCache) -> Self;

    /// Convert a value into the HalfCache type
    fn to_half_cache(self) -> HalfCache;
}

macro_rules! impl_numeric {
    ($type:ty) => {
        const _: () = assert!(
            size_of::<$type>() <= size_of::<HalfCache>(),
            "Numeric types cannot be larger than HalfCache"
        );

        impl Numeric for $type {
            fn from_half_cache(half_cache: HalfCache) -> Self {
                half_cache as $type
            }

            fn to_half_cache(self) -> HalfCache {
                self as HalfCache
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
                BITS as u32 <= HalfCache::BITS,
                "Cannot drain more than sizeof(HalfCache) bits"
            );
            assert!(
                BITS as usize <= size_of::<Type>() * 8,
                "Type must be able to hold BITS bits"
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

/// Load as many bytes as possible into a HalfCache-sized integer
fn load_partial_half_cache(data: &[u8], offset: usize) -> HalfCache {
    assert!(offset <= data.len());
    let num_bytes_left = data.len() - offset;
    let num_bytes_to_read = num_bytes_left.min(size_of::<HalfCache>());

    let mut half_cache = [0u8; size_of::<HalfCache>()];
    half_cache[..num_bytes_to_read].copy_from_slice(&data[offset..offset + num_bytes_to_read]);
    HalfCache::from_le_bytes(half_cache)
}

fn write_to_vec(vector: &mut Vec<u8>, from: &[u8], offset: usize) {
    assert!(offset <= vector.len());

    // Make sure that the new data will fit into the vector
    let end = offset + from.len();
    vector.resize(end.max(vector.len()), 0);

    vector[offset..end].copy_from_slice(from);
}

pub struct BitWriter {
    /// The bytes that have been written, excluding any bytes in the cache
    data: Vec<u8>,
    /// The position of the flushed cursor, in bytes
    flushed_position: usize,
    /// The position of the stream in bits
    position: usize,

    /// Holds any partially written data that hasn't been flushed to the data vector
    cache: Cache,
    /// The number of bits currently held in the cache
    cache_bits: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            data: vec![],
            flushed_position: 0,
            position: 0,
            cache: 0,
            cache_bits: 0,
        }
    }

    fn set_position(&mut self, position: usize) {
        self.flush_all();

        self.flushed_position = position / 8;
        self.position = position;

        let bits = position % 8;
        self.cache = Cache::from(load_partial_half_cache(&self.data, self.flushed_position));
        self.cache_bits = bits as u8;
    }

    pub fn position(&self) -> usize {
        self.position
    }

    /// If the cache has `>= HalfCache::BITS` bits, write those bits to
    /// `self.data`
    fn flush_aligned(&mut self) {
        if HalfCache::from(self.cache_bits) >= HalfCache::BITS {
            let mask = HalfCache::MAX;
            let half_cache = (self.cache & Cache::from(mask)) as HalfCache;
            write_to_vec(
                &mut self.data,
                &half_cache.to_le_bytes(),
                self.flushed_position,
            );
            self.cache >>= HalfCache::BITS;
            self.cache_bits -= HalfCache::BITS as u8;
            self.flushed_position += size_of::<HalfCache>();
        }
    }

    /// Flush all bits in the cache to `data`. Any data that is not byte-aligned
    /// will be padded with zeroes.
    /// NOTE: Will corrupt the state of the cache, so make sure to reset state
    /// if you call this.
    fn flush_all(&mut self) {
        self.flush_aligned();

        if self.cache_bits > 0 {
            assert!(u32::from(self.cache_bits) <= HalfCache::BITS);

            let mask = ((1 as Cache) << self.cache_bits) - 1;
            let masked_data = (self.cache & mask) as HalfCache;
            let previous_data =
                load_partial_half_cache(&self.data, self.flushed_position) & (!mask as HalfCache);

            let combined_data = previous_data | masked_data;

            let num_bytes = self.cache_bits.div_ceil(8);
            write_to_vec(
                &mut self.data,
                &combined_data.to_le_bytes()[..usize::from(num_bytes)],
                self.flushed_position,
            );
        }
    }

    pub fn write_out<const BITS: u8, Type: Numeric>(&mut self, value: Type) -> std::io::Result<()> {
        const {
            assert!(
                BITS as u32 <= HalfCache::BITS,
                "Cannot write more than sizeof(HalfCache) bits"
            );
            assert!(
                BITS as usize <= size_of::<Type>() * 8,
                "Type must be able to hold BITS bits"
            );
        }

        let value = value.to_half_cache();

        assert!(u32::from(self.cache_bits) <= HalfCache::BITS);
        // FIXME: this shouldn't be an assertion
        assert!(
            (HalfCache::BITS - value.leading_zeros()) <= u32::from(BITS),
            "value does not fit into specified number of bits"
        );

        let mask = (Cache::from(1u8) << BITS) - 1;
        // Clear out any bits that were here previously
        self.cache &= !(mask << self.cache_bits);
        self.cache |= (value as Cache) << self.cache_bits;
        self.cache_bits += BITS;
        self.position += usize::from(BITS);
        self.flush_aligned();

        Ok(())
    }

    pub fn write_bit(&mut self, value: bool) -> std::io::Result<()> {
        self.write_out::<1, u8>(value as u8)
    }

    pub fn write_bytes(&mut self, data: &[u8]) -> std::io::Result<()> {
        for byte in data {
            self.write_out::<8, u8>(*byte)?;
        }

        Ok(())
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.flush_all();

        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        {
            let mut writer = BitWriter::new();
            writer.write_out::<16, u16>(0x4241).unwrap();
            writer.write_out::<8, u8>(0x43).unwrap();
            writer.write_out::<16, u16>(0x4544).unwrap();
            writer.write_out::<8, u8>(0x46).unwrap();

            let bytes = writer.into_bytes();
            let mut reader = BitReader::new(&bytes);
            assert_eq!(reader.read_in::<16, u16>().unwrap(), 0x4241);
            assert_eq!(reader.read_in::<8, u8>().unwrap(), 0x43);
            assert_eq!(reader.read_in::<16, u16>().unwrap(), 0x4544);
            assert_eq!(reader.read_in::<8, u8>().unwrap(), 0x46);
        }

        {
            let mut writer = BitWriter::new();
            writer.write_out::<24, u32>(0x12_34_56).unwrap();
            writer.write_bit(true).unwrap();
            writer.write_out::<3, u32>(0b011).unwrap();
            writer.write_out::<9, u32>(0b1_1001_1010).unwrap();

            let some_bytes_reference: [u8; 8] = [0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48];
            writer.write_bytes(&some_bytes_reference).unwrap();

            writer.write_out::<32, u32>(0x41_42_43_44).unwrap();

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

    #[test]
    fn test_writer_flush() {
        let mut writer = BitWriter::new();
        writer.cache = 0x45_44_43_42_41;
        writer.cache_bits = 40;

        writer.flush_aligned();
        assert!(writer.data == [0x41, 0x42, 0x43, 0x44]);
        assert_eq!(writer.cache, 0x45);
        assert_eq!(writer.cache_bits, 8);

        assert!(writer.into_bytes() == [0x41, 0x42, 0x43, 0x44, 0x45]);

        let mut writer = BitWriter::new();
        writer.cache = 0x45_44_43_42_41;
        writer.cache_bits = 40;
        assert!(writer.into_bytes() == [0x41, 0x42, 0x43, 0x44, 0x45]);
    }

    #[test]
    fn test_writer_position() {
        let mut writer = BitWriter::new();
        writer.write_out::<16, u16>(0x4241).unwrap();
        assert_eq!(writer.position(), 16);
        writer.write_out::<8, u8>(0x43).unwrap();
        assert_eq!(writer.position(), 24);
        writer.write_out::<20, u32>(0x0F_45_44).unwrap();
        assert_eq!(writer.position(), 44);
    }

    #[test]
    fn test_writer_set_position() {
        let mut writer = BitWriter::new();
        writer.write_out::<20, u32>(0x4241).unwrap();
        writer.write_out::<4, u8>(0xD).unwrap();
        writer.write_out::<3, u8>(5).unwrap();
        writer.write_out::<5, u8>(0x10).unwrap();
        writer.write_out::<24, u32>(0x55_aa_55).unwrap();
        let old_position = writer.position();

        writer.set_position(20);
        writer.write_out::<4, u8>(0x02).unwrap();
        writer.write_out::<3, u8>(0x4).unwrap();

        writer.set_position(old_position);
        writer.write_out::<32, u32>(0x12345678).unwrap();
        writer.set_position(writer.position() - 16);
        writer.write_out::<16, u32>(0xf1_f2).unwrap();

        let bytes = writer.into_bytes();

        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_in::<20, u32>().unwrap(), 0x4241);
        assert_eq!(reader.read_in::<4, u8>().unwrap(), 0x02);
        assert_eq!(reader.read_in::<3, u8>().unwrap(), 0x04);
        assert_eq!(reader.read_in::<5, u8>().unwrap(), 0x10);
        assert_eq!(reader.read_in::<24, u32>().unwrap(), 0x55_aa_55);
        assert_eq!(reader.read_in::<32, u32>().unwrap(), 0xf1f2_5678);
    }
}
