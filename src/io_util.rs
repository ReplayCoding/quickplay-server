use std::io;

use crate::bitstream::BitReader;
use crate::bitstream::BitWriter;

pub fn write_string(writer: &mut BitWriter, string: &str) -> io::Result<()> {
    writer.write_bytes(string.as_bytes())?;
    writer.write_out::<8, u8>(0)?; // write NUL terminator

    io::Result::Ok(())
}

pub fn read_string(reader: &mut BitReader, max_len: usize) -> io::Result<String> {
    let mut data = vec![];
    let mut chars_read = 0;
    loop {
        let val: u8 = reader.read_in::<8, _>()?;
        if val == 0 {
            break;
        }

        data.push(val);

        chars_read += 1;
        if chars_read >= max_len {
            break;
        }
    }

    String::from_utf8(data).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })
}

const MAX_VARINT_32_BYTES: u32 = 5;
pub fn read_varint32(reader: &mut BitReader) -> io::Result<u32> {
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

pub fn write_varint32(writer: &mut BitWriter, value: u32) -> io::Result<()> {
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
    use crate::bitstream::{BitReader, BitWriter};

    #[test]
    fn test_string() {
        let mut writer = BitWriter::new();
        super::write_string(&mut writer, "TEST TEST TEST").expect("writes should work");

        let bytes = writer.into_bytes();
        let mut reader = BitReader::new(&bytes);
        super::read_string(&mut reader, 1024).expect("reads should work");
    }

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
