use crate::error::{CorruptReason, Result, StoreError};
use crate::record::{self, checksum};
use std::{fs::File, io::Write, os::unix::fs::FileExt, path::Path};

const HEADER_LEN: u64 = 8;

#[derive(Debug)]
pub struct Log {
    file: File,
    write_offset: u64,
}

impl Log {
    pub fn open(file_path: impl AsRef<Path>) -> Result<Self> {
        let file = File::options()
            .create(true)
            .read(true)
            .append(true)
            .open(file_path)?;

        let write_offset = file.metadata()?.len();

        let mut log = Log { file, write_offset };
        log.recover_torn_tail()?;

        Ok(log)
    }

    pub fn append(&mut self, bytes: &[u8]) -> Result<u64> {
        let record_offset = self.write_offset;

        let len = u32::try_from(bytes.len())
            .map_err(|_| StoreError::PayloadTooLarge { len: bytes.len() })?;
        let len_bytes = len.to_le_bytes();

        let crc = checksum(&len_bytes, bytes);
        let crc_bytes = crc.to_le_bytes();

        let mut to_write: Vec<u8> = Vec::with_capacity(HEADER_LEN as usize + bytes.len());
        to_write.extend_from_slice(&len_bytes);
        to_write.extend_from_slice(&crc_bytes);
        to_write.extend_from_slice(bytes);

        self.file.write_all(&to_write)?;

        self.sync()?;

        self.write_offset += to_write.len() as u64;

        Ok(record_offset)
    }

    pub fn read_at(&self, offset: u64) -> Result<Vec<u8>> {
        if !self.is_within_log(offset, HEADER_LEN) {
            return Err(StoreError::OffsetOutOfRange {
                offset,
                log_len: self.write_offset,
            });
        }

        let mut buf = [0u8; HEADER_LEN as usize];
        self.file.read_exact_at(&mut buf, offset)?;

        let len = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let crc = u32::from_le_bytes(buf[4..8].try_into().unwrap());

        if !self.is_within_log(offset + HEADER_LEN, len as u64) {
            return Err(StoreError::Corrupt {
                offset,
                reason: CorruptReason::LengthOutOfRange,
            });
        }

        let mut content_buf = vec![0u8; len as usize];
        self.file
            .read_exact_at(&mut content_buf, offset + HEADER_LEN)?;

        if !record::crc_matches(&buf[0..4], &content_buf, crc) {
            return Err(StoreError::Corrupt {
                offset,
                reason: CorruptReason::ChecksumMismatch,
            });
        }

        Ok(content_buf)
    }

    pub fn iter(&self) -> RecordIter<'_> {
        RecordIter {
            log: self,
            cursor: 0,
            done: false,
        }
    }

    pub fn sync(&self) -> Result<()> {
        self.file.sync_data()?;

        Ok(())
    }

    fn is_within_log(&self, start: u64, length: u64) -> bool {
        let Some(max_offset) = start.checked_add(length) else {
            return false;
        };

        self.write_offset >= max_offset
    }

    fn recover_torn_tail(&mut self) -> Result<()> {
        let mut iter = self.iter();

        let decision: Option<u64> = loop {
            match iter.next() {
                Some(Ok(_)) => continue,
                Some(Err(
                    StoreError::Corrupt {
                        offset,
                        reason: CorruptReason::LengthOutOfRange,
                    }
                    | StoreError::OffsetOutOfRange { offset, .. },
                )) => {
                    break Some(offset);
                }
                Some(Err(err)) => {
                    return Err(err);
                }
                None => break None,
            }
        };

        if let Some(offset) = decision {
            self.write_offset = offset;
            self.file.set_len(offset)?;
        }

        Ok(())
    }
}

pub struct RecordIter<'a> {
    log: &'a Log,
    cursor: u64,
    done: bool,
}

impl Iterator for RecordIter<'_> {
    type Item = Result<(u64, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.cursor >= self.log.write_offset {
            return None;
        }

        let record_offset = self.cursor;

        match self.log.read_at(self.cursor) {
            Err(err) => {
                self.done = true;
                Some(Err(err))
            }
            Ok(content) => {
                self.cursor += HEADER_LEN + content.len() as u64;
                Some(Ok((record_offset, content)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path;

    use super::*;

    fn build(records: &[&[u8]]) -> Result<(Log, tempfile::TempDir, std::path::PathBuf)> {
        let temp_dir = tempfile::tempdir()?;
        let file_path = temp_dir.path().join("file_path");
        let mut log = Log::open(&file_path)?;

        for &record in records {
            log.append(record)?;
        }

        Ok((log, temp_dir, file_path))
    }

    fn build_and_corrupt_at(offset: u64) -> Result<(Log, tempfile::TempDir, path::PathBuf)> {
        build_and_corrupt_at_with(offset, &[b"payload"])
    }

    fn build_and_corrupt_at_with(
        offset: u64,
        records: &[&[u8]],
    ) -> Result<(Log, tempfile::TempDir, path::PathBuf)> {
        let (log, temp_dir, file_path) = build(records)?;

        let file = File::options().read(true).write(true).open(&file_path)?;
        let mut to_corrupt = [0];
        file.read_exact_at(&mut to_corrupt, offset)?;
        file.write_at(&[to_corrupt[0] ^ 0xFF], offset)?;

        Ok((log, temp_dir, file_path))
    }

    mod open {
        use std::fs::metadata;

        use super::*;

        #[test]
        fn valid_log_sets_write_offset_to_file_end() -> Result<()> {
            let (_, _temp_dir, path) = build(&[b"payload"])?;
            let file_len = metadata(&path)?.len();

            assert_eq!(Log::open(path)?.write_offset, file_len);

            Ok(())
        }

        #[test]
        fn corrupted_record_refuses_open() -> Result<()> {
            // offset 19 lands in second record crc
            let (_, _temp_dir, path) =
                build_and_corrupt_at_with(19, &[b"payload", b"payload", b"payload"])?;

            let file_len_before_open = metadata(&path)?.len();

            let err = Log::open(&path).unwrap_err();
            assert!(
                matches!(
                    err,
                    StoreError::Corrupt {
                        offset: 15,
                        reason: CorruptReason::ChecksumMismatch
                    }
                ),
                "unexpected error {err:?}"
            );

            assert_eq!(metadata(path)?.len(), file_len_before_open);

            Ok(())
        }

        #[test]
        fn truncated_last_header_opens_and_trims() -> Result<()> {
            let (log, _temp_dir, path) = build(&[b"payload", b"payload"])?;
            log.file.set_len(18)?; // only write 3 bytes from last header then nothing

            let mut log = Log::open(&path)?;
            assert_eq!(log.write_offset, 15);
            assert_eq!(metadata(path)?.len(), 15);
            assert_eq!(log.append(b"payload")?, 15);

            Ok(())
        }

        #[test]
        fn truncated_last_content_opens_and_trims() -> Result<()> {
            let (log, _temp_dir, path) = build(&[b"payload", b"payload"])?;
            log.file.set_len(29)?; // remove 1 byte from the content

            let mut log = Log::open(&path)?;
            assert_eq!(log.write_offset, 15);
            assert_eq!(metadata(path)?.len(), 15);
            assert_eq!(log.append(b"payload")?, 15);

            Ok(())
        }
    }

    mod append {
        use std::fs::metadata;

        use super::*;

        #[test]
        fn append_twice_gives_correct_offsets() -> Result<()> {
            let temp_dir = tempfile::tempdir()?;
            let mut log = Log::open(temp_dir.path().join("file_path"))?;

            let bytes = &[0u8; 7];

            let offset = log.append(bytes)?;
            assert_eq!(offset, 0);

            let offset = log.append(bytes)?;
            assert_eq!(offset, HEADER_LEN + bytes.len() as u64);

            Ok(())
        }

        #[test]
        fn after_reopen_gives_correct_data_and_offset() -> Result<()> {
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
        fn correct_record() -> Result<()> {
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
        fn corrupted_len_record_returns_error() -> Result<()> {
            let (log, _temp_dir, _) = build_and_corrupt_at(0)?;

            let err = log.read_at(0).unwrap_err();

            assert!(
                matches!(
                    err,
                    StoreError::Corrupt {
                        offset: 0,
                        reason: CorruptReason::LengthOutOfRange
                    }
                ),
                "unexpected error: {err:?}"
            );

            Ok(())
        }

        #[test]
        fn corrupted_crc_record_returns_error() -> Result<()> {
            let (log, _temp_dir, _) = build_and_corrupt_at(4)?;

            let err = log.read_at(0).unwrap_err();

            assert!(
                matches!(
                    err,
                    StoreError::Corrupt {
                        offset: 0,
                        reason: CorruptReason::ChecksumMismatch
                    }
                ),
                "unexpected error: {err:?}"
            );

            Ok(())
        }

        #[test]
        fn corrupted_content_record_returns_error() -> Result<()> {
            let (log, _temp_dir, _) = build_and_corrupt_at(8)?;

            let err = log.read_at(0).unwrap_err();

            assert!(
                matches!(
                    err,
                    StoreError::Corrupt {
                        offset: 0,
                        reason: CorruptReason::ChecksumMismatch
                    }
                ),
                "unexpected error: {err:?}"
            );

            Ok(())
        }

        #[test]
        fn offset_out_of_range_returns_error() -> Result<()> {
            let temp_dir = tempfile::tempdir()?;
            let log = Log::open(temp_dir.path().join("file_path"))?;

            let err = log.read_at(8).unwrap_err();

            assert!(
                matches!(
                    err,
                    StoreError::OffsetOutOfRange {
                        offset: 8,
                        log_len: 0
                    }
                ),
                "unexpected error: {err:?}"
            );

            Ok(())
        }
    }

    mod iter {
        use super::*;

        #[test]
        fn over_empty_log_returns_none() -> Result<()> {
            let (log, _temp_dir, _) = build(&[])?;

            assert!(log.iter().next().is_none());

            Ok(())
        }

        #[test]
        fn over_3_records_returns_correct_contents() -> Result<()> {
            let records: &[&[u8]] = &[b"first", b"second", b"third"];
            let offsets = [0, 13, 27];
            let (log, _temp_dir, _) = build(records)?;

            let mut iter = log.iter();
            for (expected_record, expected_offset) in records.iter().zip(offsets) {
                let (offset, record) = iter.next().expect("iterator ended early")?;
                assert_eq!(record, *expected_record);
                assert_eq!(offset, expected_offset);
            }

            let next = iter.next();
            assert!(next.is_none(), "unexpected next: {next:?}");

            Ok(())
        }

        #[test]
        fn over_records_with_2nd_corrupted_returns_valid_then_error() -> Result<()> {
            let records: &[&[u8]] = &[b"first", b"second"];
            let (log, _temp_dir, _) = build_and_corrupt_at_with(17, records)?;
            let mut iter = log.iter();

            assert_eq!(
                iter.next().expect("iterator ended early")?,
                (0u64, b"first".to_vec())
            );

            let err = iter.next().expect("expected error").unwrap_err();
            assert!(
                matches!(
                    err,
                    StoreError::Corrupt {
                        offset: 13,
                        reason: CorruptReason::ChecksumMismatch
                    }
                ),
                "unexpected error: {err:?}"
            );

            let next = iter.next();
            assert!(next.is_none(), "unexpected next: {next:?}");

            Ok(())
        }
    }
}
