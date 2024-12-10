use super::bitstream::{BitReader, BitStreamError, BitWriter};

const MAX_VARINT_32_BYTES: u32 = 5;
pub fn read_varint32(reader: &mut BitReader) -> Result<u32, BitStreamError> {
    let mut result: u32 = 0;
    let mut count: u32 = 0;

    loop {
        if count == MAX_VARINT_32_BYTES {
            return Ok(result);
        }

        let b: u32 = reader.read_in::<8, _>()?;
        result |= (b & 0x7f) << (7 * count);

        count += 1;

        if (b & 0x80) == 0 {
            break;
        }
    }

    Ok(result)
}

pub fn write_varint32(writer: &mut BitWriter, value: u32) -> Result<(), BitStreamError> {
    let mut value = value;

    while value > 0x7f {
        writer.write_out::<8, _>((value & 0x7f) | 0x80)?;
        value >>= 7;
    }

    writer.write_out::<8, _>(value & 0x7f)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::io::bitstream::{BitReader, BitWriter};

    #[test]
    fn test_varint32() {
        let numbers: &[(u32, &'static [u8])] = &[
            (0x00, &[0x00]),
            (0x01, &[0x01]),
            (0x7F, &[0x7F]),
            (0x3FFF, &[0xFF, 0x7F]),
            (0x1F_FFFF, &[0xFF, 0xFF, 0x7F]),
            (0xFFF_FFFF, &[0xFF, 0xFF, 0xFF, 0x7F]),
            (u32::MAX, &[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]),
        ];

        for (expected, bytes) in numbers {
            let mut reader = BitReader::new(bytes);
            assert_eq!(super::read_varint32(&mut reader).unwrap(), *expected);
        }

        // excess bytes should be ignored
        let mut reader = BitReader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x41]);
        assert_eq!(super::read_varint32(&mut reader).unwrap(), 0xFFFF_FFFF);

        for (number, expected) in numbers {
            let mut writer = BitWriter::new();
            super::write_varint32(&mut writer, *number).unwrap();

            assert_eq!(*expected, writer.into_bytes());
        }
    }
}
