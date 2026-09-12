use crate::record::checksum;
use std::{
    fs::File,
    io::{self, Error, ErrorKind, Write},
    os::unix::fs::FileExt,
    path::Path,
};

struct Log {
    file: File,
    write_offset: u64,
}

impl Log {
    pub fn open(file_path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = File::options()
            .create(true)
            .read(true)
            .append(true)
            .open(file_path)?;

        let write_offset = file.metadata()?.len();

        Ok(Log { file, write_offset })
    }

    pub fn append(&mut self, bytes: &[u8]) -> std::io::Result<u64> {
        let record_offset = self.write_offset;

        let len = u32::try_from(bytes.len()).map_err(io::Error::other)?;
        let len_bytes = len.to_le_bytes();

        let crc = checksum(&len_bytes, bytes);
        let crc_bytes = crc.to_le_bytes();

        let mut to_write: Vec<u8> = Vec::with_capacity(8 + bytes.len());
        to_write.extend_from_slice(&len_bytes);
        to_write.extend_from_slice(&crc_bytes);
        to_write.extend_from_slice(bytes);

        self.file.write_all(&to_write)?;

        self.write_offset += to_write.len() as u64;

        Ok(record_offset)
    }

    pub fn read_at(&self, offset: u64) -> io::Result<Vec<u8>> {
        self.check_file_space(offset, 8)?;

        let mut buf = [0u8; 8];
        self.file.read_exact_at(&mut buf, offset)?;

        let len = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let crc = u32::from_le_bytes(buf[4..8].try_into().unwrap());

        self.check_file_space(offset + 8, len as u64)?;

        let mut content_buf = vec![0u8; len as usize];
        self.file.read_exact_at(&mut content_buf, offset + 8)?;

        Self::check_crc(&buf[0..4], &content_buf, crc)?;

        Ok(content_buf)
    }

    fn check_file_space(&self, start: u64, length: u64) -> std::io::Result<()> {
        let Some(max_offset) = start.checked_add(length) else {
            return Err(Error::new(ErrorKind::InvalidData, "length too long"));
        };

        if self.write_offset >= max_offset {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                "trying to read past EOF",
            ))
        }
    }

    fn check_crc(len_bytes: &[u8], bytes: &[u8], crc: u32) -> std::io::Result<()> {
        if crc == checksum(len_bytes, bytes) {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::InvalidData, "crc mismatch"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod append {
        use std::fs::metadata;

        use super::*;

        #[test]
        fn append_twice_gives_correct_offsets() -> std::io::Result<()> {
            let temp_dir = tempfile::tempdir()?;
            let mut log = Log::open(temp_dir.path().join("file_path"))?;

            let bytes = &[0u8; 7];

            let offset = log.append(bytes)?;
            assert_eq!(offset, 0);

            let offset = log.append(bytes)?;
            assert_eq!(offset, (8 + bytes.len()) as u64);

            Ok(())
        }

        #[test]
        fn after_reopen_gives_correct_data_and_offset() -> std::io::Result<()> {
            let temp_dir = tempfile::tempdir()?;
            let file_path = temp_dir.path().join("file_path");

            let mut log = Log::open(&file_path)?;
            let bytes = &[1u8; 7];
            log.append(bytes)?;
            drop(log);

            let mut reopened_log = Log::open(&file_path)?;
            assert_eq!(reopened_log.read_at(0)?, bytes);

            let offset = reopened_log.append(bytes)?;
            assert_eq!(offset, 15);
            assert_eq!(metadata(&file_path)?.len(), 30);

            Ok(())
        }
    }

    mod read_at {
        use super::*;

        #[test]
        fn correct_record() -> std::io::Result<()> {
            let temp_dir = tempfile::tempdir()?;
            let mut log = Log::open(temp_dir.path().join("file_path"))?;
            let data = b"payload";

            // First test with offset = 0
            let offset = log.append(data)?;
            assert_eq!(log.read_at(offset)?, data);

            // Second test with offset > 0
            let offset = log.append(data)?;
            assert_eq!(log.read_at(offset)?, data);

            Ok(())
        }

        #[test]
        fn corrupted_len_record_returns_error() -> std::io::Result<()> {
            let (log, _temp_dir) = build_and_corrupt_at(0)?;

            assert!(log.read_at(0).is_err());

            Ok(())
        }

        #[test]
        fn corrupted_crc_record_returns_error() -> std::io::Result<()> {
            let (log, _temp_dir) = build_and_corrupt_at(4)?;

            assert!(log.read_at(0).is_err());

            Ok(())
        }

        #[test]
        fn corrupted_content_record_returns_error() -> std::io::Result<()> {
            let (log, _temp_dir) = build_and_corrupt_at(8)?;

            assert!(log.read_at(0).is_err());

            Ok(())
        }

        fn build_and_corrupt_at(offset: u64) -> std::io::Result<(Log, tempfile::TempDir)> {
            let temp_dir = tempfile::tempdir()?;
            let file_path = temp_dir.path().join("file_path");
            let mut log = Log::open(&file_path)?;
            let data = b"payload";
            log.append(data)?;

            let file = File::options().read(true).write(true).open(&file_path)?;
            let mut to_corrupt = [0];
            file.read_exact_at(&mut to_corrupt, offset)?;
            file.write_at(&[to_corrupt[0] ^ 0xFF], offset)?;

            Ok((log, temp_dir))
        }
    }
}
