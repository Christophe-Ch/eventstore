use std::{collections::HashMap, path::Path};

use crate::{
    error::{CorruptReason, Result, StoreError},
    event::Event,
    log::Log,
};

#[derive(Debug)]
pub struct Store {
    log: Log,
    index: HashMap<String, Vec<u64>>,
}

impl Store {
    pub fn open(file_path: impl AsRef<Path>) -> Result<Store> {
        let log = Log::open(file_path)?;
        let mut index: HashMap<String, Vec<u64>> = HashMap::new();

        for record in log.iter() {
            let (offset, payload) = record?;
            let Event {
                stream, version, ..
            } = Event::decode(&payload).map_err(|err| StoreError::Corrupt {
                offset,
                reason: CorruptReason::MalformedFrame(err),
            })?;

            let offsets = index.get(&stream);
            let expected_version = offsets.map_or(0, |offsets| offsets.len() as u64);

            if version != expected_version {
                return Err(StoreError::Corrupt {
                    offset,
                    reason: CorruptReason::NonContiguousVersion {
                        expected: expected_version,
                        found: version,
                    },
                });
            }

            index.entry(stream).or_default().push(offset);
        }

        Ok(Store { log, index })
    }

    pub fn stream_version(&self, stream: &str) -> Option<u64> {
        self.index
            .get(stream)
            .map(|offsets| offsets.len() as u64 - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens a log in a temp dir, appends each event's encoded frame, and hands
    /// back the path plus the temp dir (which must stay alive for the test).
    fn build(events: &[Event]) -> Result<(tempfile::TempDir, std::path::PathBuf)> {
        let temp_dir = tempfile::tempdir()?;
        let file_path = temp_dir.path().join("test");

        let mut log = Log::open(&file_path)?;
        for event in events {
            log.append(&event.encode()?)?;
        }

        Ok((temp_dir, file_path))
    }

    /// Appends a payload straight to the log, bypassing `Event::encode`, so a
    /// frame the decoder rejects can be planted in an otherwise valid log.
    fn append_raw(file_path: &std::path::Path, payload: &[u8]) -> Result<u64> {
        Log::open(file_path)?.append(payload)
    }

    mod open {
        use std::{fs::File, io::Write};

        use crate::event::MalformedEvent;

        use super::*;

        #[test]
        fn empty_log_yields_an_empty_index() -> Result<()> {
            let (_temp_dir, file_path) = build(&[])?;

            let store = Store::open(file_path)?;
            assert_eq!(store.index.len(), 0);

            Ok(())
        }

        #[test]
        fn maps_a_stream_to_its_offsets_in_log_order() -> Result<()> {
            let events = [
                Event::new(String::from("orders-1"), 0, b"hi".to_vec()),
                Event::new(String::from("orders-1"), 1, b"hello".to_vec()),
            ];
            let expected_offsets = [0, 28];
            let (_temp_dir, file_path) = build(&events)?;

            let store = Store::open(file_path)?;
            assert_eq!(store.index.len(), 1);

            let offsets = store.index.get("orders-1").expect("orders-1 indexed");
            assert_eq!(offsets, &expected_offsets);

            Ok(())
        }

        #[test]
        fn keeps_per_stream_order_when_streams_interleave() -> Result<()> {
            let events = [
                Event::new(String::from("orders-1"), 0, b"hi".to_vec()),
                Event::new(String::from("accounts-1"), 0, b"hey".to_vec()),
                Event::new(String::from("orders-1"), 1, b"hello".to_vec()),
            ];
            let expected_orders_offsets = [0, 59];
            let expected_accounts_offsets = [28];
            let (_temp_dir, file_path) = build(&events)?;

            let store = Store::open(file_path)?;
            assert_eq!(store.index.len(), 2);

            let orders_offsets = store.index.get("orders-1").expect("orders-1 indexed");
            let accounts_offsets = store.index.get("accounts-1").expect("accounts-1 indexed");
            assert_eq!(orders_offsets, &expected_orders_offsets);
            assert_eq!(accounts_offsets, &expected_accounts_offsets);

            Ok(())
        }

        #[test]
        fn rejects_a_malformed_frame() -> Result<()> {
            let (_temp_dir, file_path) =
                build(&[Event::new(String::from("orders-1"), 0, b"hi".to_vec())])?;
            append_raw(&file_path, &[0x00])?;
            append_raw(
                &file_path,
                &Event::new(String::from("orders-1"), 1, b"hi".to_vec()).encode()?,
            )?;

            assert!(matches!(
                Store::open(file_path).unwrap_err(),
                StoreError::Corrupt {
                    offset: 28,
                    reason: CorruptReason::MalformedFrame(MalformedEvent::TooShort)
                }
            ));

            Ok(())
        }

        #[test]
        fn rejects_a_first_event_that_is_not_version_zero() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 1, vec![])])?;

            let err = Store::open(file_path).unwrap_err();
            assert!(matches!(
                err,
                StoreError::Corrupt {
                    offset: 0,
                    reason: CorruptReason::NonContiguousVersion {
                        expected: 0,
                        found: 1
                    }
                }
            ));

            Ok(())
        }

        #[test]
        fn rejects_a_gap_in_versions() -> Result<()> {
            let (_temp_dir, file_path) = build(&[
                Event::new(String::from("orders-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 2, vec![]),
            ])?;

            let err = Store::open(file_path).unwrap_err();
            assert!(matches!(
                err,
                StoreError::Corrupt {
                    offset: 26,
                    reason: CorruptReason::NonContiguousVersion {
                        expected: 1,
                        found: 2
                    }
                }
            ));

            Ok(())
        }

        #[test]
        fn rejects_a_repeated_version() -> Result<()> {
            let (_temp_dir, file_path) = build(&[
                Event::new(String::from("orders-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 0, vec![]),
            ])?;

            let err = Store::open(file_path).unwrap_err();
            assert!(matches!(
                err,
                StoreError::Corrupt {
                    offset: 26,
                    reason: CorruptReason::NonContiguousVersion {
                        expected: 1,
                        found: 0
                    }
                }
            ));

            Ok(())
        }

        #[test]
        fn truncates_a_torn_tail_before_rebuilding() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 0, vec![])])?;
            File::options()
                .append(true)
                .open(&file_path)?
                .write_all(&[0x00])?;

            let store = Store::open(file_path)?;
            assert_eq!(store.index.len(), 1);

            let offsets = store.index.get("orders-1").expect("orders-1 indexed");
            assert_eq!(offsets, &[0]);

            Ok(())
        }
    }

    mod stream_version {
        use super::*;

        #[test]
        fn returns_the_last_version() -> Result<()> {
            let (_temp_dir, file_path) = build(&[
                Event::new(String::from("orders-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 1, vec![]),
            ])?;

            assert_eq!(Store::open(file_path)?.stream_version("orders-1"), Some(1));

            Ok(())
        }

        #[test]
        fn is_none_for_an_unknown_stream() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 0, vec![])])?;

            assert_eq!(Store::open(file_path)?.stream_version("accounts-1"), None);

            Ok(())
        }
    }
}
