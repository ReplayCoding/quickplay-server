use bitstream_io::{BitRead, BitWrite};

pub fn write_string<W: BitWrite>(reader: &mut W, string: &str) -> std::io::Result<()> {
    reader.write_bytes(string.as_bytes())?;
    reader.write_out::<8, _>(0)?; // write NUL terminator

    std::io::Result::Ok(())
}

pub fn _read_string<R: BitRead>(reader: &mut R, max_len: usize) -> anyhow::Result<String> {
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
