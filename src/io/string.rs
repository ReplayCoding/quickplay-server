use super::bitstream::{BitReader, BitStreamError, BitWriter};

pub fn write_string(writer: &mut BitWriter, string: &str) -> Result<(), BitStreamError> {
    writer.write_bytes(string.as_bytes())?;
    writer.write_out::<8, u8>(0)?; // write NUL terminator

    Ok(())
}

pub fn read_string(reader: &mut BitReader, max_len: usize) -> Result<String, BitStreamError> {
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

    String::from_utf8(data).map_err(|_| BitStreamError::InvalidUtf8)
}

#[cfg(test)]
mod tests {
    use crate::io::bitstream::{BitReader, BitWriter};

    #[test]
    fn test_string() {
        let mut writer = BitWriter::new();
        super::write_string(&mut writer, "TEST TEST TEST").expect("writes should work");

        let bytes = writer.into_bytes();
        let mut reader = BitReader::new(&bytes);
        super::read_string(&mut reader, 1024).expect("reads should work");
    }
}
