use crc::{CRC_32_ISCSI, Crc};

const CRC32C: Crc<u32> = Crc::<u32>::new(&CRC_32_ISCSI);

pub fn checksum(len_bytes: &[u8], bytes: &[u8]) -> u32 {
    let mut digest = CRC32C.digest();

    digest.update(len_bytes);
    digest.update(bytes);

    digest.finalize()
}

pub fn crc_matches(len_bytes: &[u8], bytes: &[u8], crc: u32) -> bool {
    crc == checksum(len_bytes, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_checksum() {
        let payload = b"payload";
        let payload_len = 7u32.to_le_bytes();

        assert_eq!(checksum(&payload_len, payload), 0x0F6DE9B1);
    }
}
