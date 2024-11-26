use thiserror::Error;

#[derive(Error, Debug)]
pub enum CompressionError {
    #[error("no compression type found")]
    NoCompressionType,
    #[error("unknown compression type: {0:02x?}")]
    UnhandledCompressionType([u8; 4]),
    #[error("no compressed data found")]
    NoCompressedData,
    #[error("snappy error: {0:?}")]
    Snappy(snap::Error),
    #[error("buffer of size {0} is too large to compress")]
    TooLarge(usize),
}

const COMPRESSION_SNAPPY: &[u8] = b"SNAP";

pub fn decompress(data: &[u8]) -> Result<Vec<u8>, CompressionError> {
    let compression_type = data.get(0..4).ok_or(CompressionError::NoCompressionType)?;

    match compression_type {
        COMPRESSION_SNAPPY => {
            let compressed_data = data.get(4..).ok_or(CompressionError::NoCompressedData)?;

            let mut decoder = snap::raw::Decoder::new();
            decoder
                .decompress_vec(compressed_data)
                .map_err(CompressionError::Snappy)
        }

        _ => Err(CompressionError::UnhandledCompressionType(
            // .unwrap() will never fail because compression_type is always
            // exactly 4 bytes
            compression_type.try_into().unwrap(),
        )),
    }
}

pub fn compress(data: &[u8]) -> Result<Vec<u8>, CompressionError> {
    const HEADER_SIZE: usize = 4;
    let max_compressed_size = snap::raw::max_compress_len(data.len());
    if max_compressed_size == 0 {
        return Err(CompressionError::TooLarge(data.len()));
    }

    let mut buffer = vec![0u8; HEADER_SIZE + max_compressed_size];
    buffer[0..HEADER_SIZE].copy_from_slice(COMPRESSION_SNAPPY);

    let mut encoder = snap::raw::Encoder::new();
    let bytes_used = encoder
        .compress(data, &mut buffer[HEADER_SIZE..])
        .map_err(CompressionError::Snappy)?;

    buffer.truncate(HEADER_SIZE + bytes_used);
    Ok(buffer)
}
