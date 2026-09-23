use std::{collections::HashMap, iter::FusedIterator, path::Path, slice::Iter};

use crate::{
    error::{
        CorruptReason::{self, MalformedFrame},
        Result, StoreError,
    },
    event::Event,
    log::Log,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedVersion {
    NoStream,
    Exact(u64),
}

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

    pub fn read_stream(&self, stream: &str) -> StreamIter<'_> {
        StreamIter::new(
            &self.log,
            self.index
                .get(stream)
                .map_or([].as_slice(), |offsets| offsets.as_slice())
                .iter(),
        )
    }

    pub fn append(
        &mut self,
        stream: &str,
        expected_version: ExpectedVersion,
        data: Vec<u8>,
    ) -> Result<u64> {
        let current_version = self.stream_version(stream);
        let expected_version = match expected_version {
            ExpectedVersion::NoStream => None,
            ExpectedVersion::Exact(version) => Some(version),
        };

        if current_version != expected_version {
            return Err(StoreError::UnexpectedVersion {
                expected: expected_version,
                got: current_version,
            });
        }

        let new_version = current_version.map_or(0, |v| v + 1);
        let offset = self
            .log
            .append(&Event::new(stream.to_string(), new_version, data).encode()?)?;
        self.index
            .entry(stream.to_string())
            .or_default()
            .push(offset);
        Ok(new_version)
    }
}

pub struct StreamIter<'a> {
    log: &'a Log,
    offsets: Iter<'a, u64>,
    failed: bool,
}

impl<'a> StreamIter<'a> {
    fn new(log: &'a Log, offsets: Iter<'a, u64>) -> Self {
        StreamIter {
            log,
            offsets,
            failed: false,
        }
    }
}

impl FusedIterator for StreamIter<'_> {}
impl Iterator for StreamIter<'_> {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }

        match self.offsets.next() {
            None => None,
            Some(offset) => Some(
                self.log
                    .read_at(*offset)
                    .and_then(|bytes| {
                        Event::decode(&bytes).map_err(|err| StoreError::Corrupt {
                            offset: *offset,
                            reason: MalformedFrame(err),
                        })
                    })
                    .inspect_err(|_| {
                        self.failed = true;
                    }),
            ),
        }
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

    mod read_stream {
        use std::{fs::File, os::unix::fs::FileExt};

        use super::*;

        /// Flips a byte inside the record at `record_offset`, through a handle
        /// the `Store` does not own, so damage appears after the rebuild.
        fn corrupt_payload_at(file_path: &std::path::Path, record_offset: u64) -> Result<()> {
            let file = File::options().write(true).read(true).open(file_path)?;
            let mut byte = [0u8; 1];
            let payload_offset = record_offset + 8;
            file.read_exact_at(&mut byte, payload_offset)?;
            file.write_all_at(&[byte[0] ^ 0xFF], payload_offset)?;

            Ok(())
        }

        #[test]
        fn returns_only_the_requested_stream() -> Result<()> {
            let events = [
                Event::new(String::from("orders-1"), 0, b"hi".to_vec()),
                Event::new(String::from("accounts-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 1, b"hello".to_vec()),
            ];
            let orders_events = [
                Event::new(String::from("orders-1"), 0, b"hi".to_vec()),
                Event::new(String::from("orders-1"), 1, b"hello".to_vec()),
            ];
            let (_temp_dir, file_path) = build(&events)?;

            let store = Store::open(file_path)?;
            let orders_stream = store.read_stream("orders-1");

            assert_eq!(orders_stream.collect::<Result<Vec<_>>>()?, orders_events);

            Ok(())
        }

        #[test]
        fn is_empty_for_an_unknown_stream() -> Result<()> {
            let events = [Event::new(String::from("orders-1"), 0, b"hi".to_vec())];
            let (_temp_dir, file_path) = build(&events)?;

            let store = Store::open(file_path)?;
            let accounts_stream = store.read_stream("accounts-1");

            assert!(accounts_stream.collect::<Result<Vec<_>>>()?.is_empty());

            Ok(())
        }

        #[test]
        fn reads_an_event_with_no_data() -> Result<()> {
            let events = [Event::new(String::from("orders-1"), 0, vec![])];
            let (_temp_dir, file_path) = build(&events)?;

            let store = Store::open(file_path)?;
            let mut orders_stream = store.read_stream("orders-1");

            assert!(orders_stream.next().unwrap()?.data.is_empty());

            Ok(())
        }

        #[test]
        fn reports_corruption_at_the_offset() -> Result<()> {
            let (_temp_dir, file_path) = build(&[
                Event::new(String::from("orders-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 1, vec![]),
            ])?;

            let store = Store::open(&file_path)?;
            let mut orders_stream = store.read_stream("orders-1");

            corrupt_payload_at(&file_path, 26)?;

            orders_stream.next();
            let err = orders_stream.next().unwrap().unwrap_err();
            assert!(
                matches!(
                    err,
                    StoreError::Corrupt {
                        offset: 26,
                        reason: CorruptReason::ChecksumMismatch
                    }
                ),
                "unexpected error {err:?}"
            );

            Ok(())
        }

        #[test]
        fn yields_nothing_after_an_error() -> Result<()> {
            let (_temp_dir, file_path) = build(&[
                Event::new(String::from("orders-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 1, vec![]),
            ])?;

            let store = Store::open(&file_path)?;
            let mut orders_stream = store.read_stream("orders-1");

            corrupt_payload_at(&file_path, 0)?;

            assert!(orders_stream.next().unwrap().is_err());
            assert!(orders_stream.next().is_none());

            Ok(())
        }

        #[test]
        fn does_not_read_past_what_the_caller_takes() -> Result<()> {
            let events = [
                Event::new(String::from("orders-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 1, vec![]),
            ];
            let (_temp_dir, file_path) = build(&events)?;

            let store = Store::open(&file_path)?;
            let orders_stream = store.read_stream("orders-1");

            corrupt_payload_at(&file_path, 26)?;

            assert_eq!(
                orders_stream.take(1).collect::<Result<Vec<_>>>()?,
                &events[..1]
            );

            Ok(())
        }
    }
    mod append {
        use super::*;

        #[test]
        fn assigns_version_zero_to_a_new_stream() -> Result<()> {
            let (_temp_dir, file_path) = build(&[])?;
            let mut store = Store::open(&file_path)?;

            assert_eq!(
                store.append("new-stream", ExpectedVersion::NoStream, vec![])?,
                0
            );

            Ok(())
        }

        #[test]
        fn assigns_the_next_version_to_an_existing_stream() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 0, vec![])])?;
            let mut store = Store::open(&file_path)?;

            assert_eq!(
                store.append("orders-1", ExpectedVersion::Exact(0), vec![])?,
                1
            );

            Ok(())
        }

        #[test]
        fn rejects_no_stream_when_the_stream_exists() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 0, vec![])])?;
            let mut store = Store::open(&file_path)?;

            let err = store
                .append("orders-1", ExpectedVersion::NoStream, vec![])
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    StoreError::UnexpectedVersion {
                        expected: None,
                        got: Some(0)
                    }
                ),
                "unexpected err {err:?}"
            );

            Ok(())
        }

        #[test]
        fn rejects_exact_when_the_stream_does_not_exist() -> Result<()> {
            let (_temp_dir, file_path) = build(&[])?;
            let mut store = Store::open(&file_path)?;

            let err = store
                .append("orders-1", ExpectedVersion::Exact(0), vec![])
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    StoreError::UnexpectedVersion {
                        expected: Some(0),
                        got: None
                    }
                ),
                "unexpected err {err:?}"
            );

            Ok(())
        }

        #[test]
        fn rejects_a_stale_expected_version() -> Result<()> {
            let (_temp_dir, file_path) = build(&[
                Event::new(String::from("orders-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 1, vec![]),
            ])?;
            let mut store = Store::open(&file_path)?;

            let err = store
                .append("orders-1", ExpectedVersion::Exact(0), vec![])
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    StoreError::UnexpectedVersion {
                        expected: Some(0),
                        got: Some(1)
                    }
                ),
                "unexpected err {err:?}"
            );

            Ok(())
        }

        #[test]
        fn rejects_an_expected_version_ahead_of_the_stream() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 0, vec![])])?;
            let mut store = Store::open(&file_path)?;

            let err = store
                .append("orders-1", ExpectedVersion::Exact(1), vec![])
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    StoreError::UnexpectedVersion {
                        expected: Some(1),
                        got: Some(0)
                    }
                ),
                "unexpected err {err:?}"
            );

            Ok(())
        }

        #[test]
        fn makes_the_appended_event_readable() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 0, vec![])])?;

            let mut store = Store::open(&file_path)?;
            store.append("orders-1", ExpectedVersion::Exact(0), vec![])?;

            let stream = store.read_stream("orders-1");
            assert_eq!(
                stream.collect::<Result<Vec<_>>>()?,
                vec![
                    Event::new(String::from("orders-1"), 0, vec![]),
                    Event::new(String::from("orders-1"), 1, vec![])
                ]
            );

            Ok(())
        }

        #[test]
        fn a_conflict_writes_nothing() -> Result<()> {
            let (_temp_dir, file_path) = build(&[
                Event::new(String::from("orders-1"), 0, vec![]),
                Event::new(String::from("orders-1"), 1, vec![]),
            ])?;

            let mut store = Store::open(&file_path)?;
            store
                .append("orders-1", ExpectedVersion::Exact(0), vec![])
                .unwrap_err();
            drop(store);

            let store = Store::open(file_path)?;
            let stream = store.read_stream("orders-1");
            assert_eq!(
                stream.collect::<Result<Vec<_>>>()?,
                vec![
                    Event::new(String::from("orders-1"), 0, vec![]),
                    Event::new(String::from("orders-1"), 1, vec![])
                ]
            );

            Ok(())
        }

        #[test]
        fn an_appended_event_survives_a_reopen() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 0, vec![])])?;

            let mut store = Store::open(&file_path)?;
            store.append("orders-1", ExpectedVersion::Exact(0), vec![])?;
            drop(store);

            let store = Store::open(&file_path)?;
            let stream = store.read_stream("orders-1");
            assert_eq!(
                stream.collect::<Result<Vec<_>>>()?,
                vec![
                    Event::new(String::from("orders-1"), 0, vec![]),
                    Event::new(String::from("orders-1"), 1, vec![])
                ]
            );

            Ok(())
        }

        #[test]
        fn appends_to_a_second_stream_independently() -> Result<()> {
            let (_temp_dir, file_path) = build(&[Event::new(String::from("orders-1"), 0, vec![])])?;

            let mut store = Store::open(file_path)?;
            store.append("accounts-1", ExpectedVersion::NoStream, vec![])?;

            assert_eq!(
                store.read_stream("orders-1").collect::<Result<Vec<_>>>()?,
                vec![Event::new(String::from("orders-1"), 0, vec![]),]
            );

            assert_eq!(
                store
                    .read_stream("accounts-1")
                    .collect::<Result<Vec<_>>>()?,
                vec![Event::new(String::from("accounts-1"), 0, vec![]),]
            );

            Ok(())
        }
    }
}
