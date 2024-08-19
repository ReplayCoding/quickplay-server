use bitstream_io::{BitRead, BitWrite};

pub fn write_string<W: BitWrite>(reader: &mut W, string: &str) -> std::io::Result<()> {
    reader.write_bytes(string.as_bytes())?;
    reader.write_out::<8, _>(0)?; // write NUL terminator

    std::io::Result::Ok(())
}

pub fn read_string<R: BitRead>(reader: &mut R, max_len: usize) -> anyhow::Result<String> {
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

    Ok(String::from_utf8(data)?)
}

const MAX_VARINT_32_BYTES: u32 = 5;
pub fn read_varint32<R: BitRead>(reader: &mut R) -> anyhow::Result<u32> {
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

pub fn write_varint32<W: BitWrite>(writer: &mut W, value: u32) -> anyhow::Result<()> {
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
    use bitstream_io::{BitReader, BitWriter, LittleEndian};
    use std::io::Cursor;

    #[test]
    fn test_string() {
        let mut buffer: Vec<u8> = vec![];

        let mut writer = BitWriter::endian(Cursor::new(&mut buffer), LittleEndian);
        super::write_string(&mut writer, "TEST TEST TEST").expect("writes should work");

        let mut reader = BitReader::endian(Cursor::new(&mut buffer), LittleEndian);
        super::read_string(&mut reader, 1024).expect("reads should work");
    }

    #[test]
    fn test_varint32() {
        let mut bytes: Vec<u8> = vec![];
        for i in (0..u32::MAX).step_by(8192) {
            bytes.clear();

            let mut writer = BitWriter::endian(Cursor::new(&mut bytes), LittleEndian);
            super::write_varint32(&mut writer, i).unwrap();

            let mut reader = BitReader::endian(Cursor::new(&bytes), LittleEndian);
            assert_eq!(super::read_varint32(&mut reader).unwrap(), i);
        }
    }
}
