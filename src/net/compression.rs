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
}

const COMPRESSION_SNAPPY: &[u8] = b"SNAP";

pub fn decompress(data: &[u8]) -> Result<Vec<u8>, CompressionError> {
    let compression_type = data
        .get(0..4)
        .ok_or(CompressionError::NoCompressionType)?;

    match compression_type {
        COMPRESSION_SNAPPY => {
            let compressed_data = data
                .get(4..)
                .ok_or(CompressionError::NoCompressedData)?;

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
