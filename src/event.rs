use std::fmt::Display;

use crate::error::{Result, StoreError};

#[derive(Debug, PartialEq)]
pub struct Event {
    stream: String,
    version: u64,
    data: Vec<u8>,
}

const STREAM_LEN_LEN: usize = 2;
const VERSION_LEN: usize = 8;
const FIXED_HEADER_LEN: usize = STREAM_LEN_LEN + VERSION_LEN;

impl Event {
    pub fn new(stream: String, version: u64, data: Vec<u8>) -> Self {
        Event {
            stream,
            version,
            data,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.stream.is_empty() {
            return Err(StoreError::EmptyStreamId);
        }

        let stream_len =
            u16::try_from(self.stream.len()).map_err(|_| StoreError::StreamIdTooLong {
                len: self.stream.len(),
            })?;
        let stream_len_bytes = stream_len.to_le_bytes();

        let mut buffer =
            Vec::<u8>::with_capacity(FIXED_HEADER_LEN + self.stream.len() + self.data.len());
        buffer.extend_from_slice(&stream_len_bytes);
        buffer.extend_from_slice(&self.version.to_le_bytes());
        buffer.extend_from_slice(self.stream.as_bytes());
        buffer.extend_from_slice(&self.data);

        Ok(buffer)
    }

    pub fn decode(bytes: &[u8]) -> std::result::Result<Event, MalformedEvent> {
        if bytes.len() < FIXED_HEADER_LEN {
            return Err(MalformedEvent::TooShort);
        }

        let stream_len_bytes = &bytes[..STREAM_LEN_LEN];
        let stream_len = u16::from_le_bytes(stream_len_bytes.try_into().unwrap());

        if stream_len == 0 {
            return Err(MalformedEvent::StreamIdLengthZero);
        }

        let version_bytes = &bytes[STREAM_LEN_LEN..FIXED_HEADER_LEN];
        let version = u64::from_le_bytes(version_bytes.try_into().unwrap());

        if bytes.len() < FIXED_HEADER_LEN + stream_len as usize {
            return Err(MalformedEvent::StreamIdOutOfRange);
        }

        let stream_bytes = &bytes[FIXED_HEADER_LEN..(FIXED_HEADER_LEN + stream_len as usize)];
        let stream = str::from_utf8(stream_bytes)
            .map_err(|_| MalformedEvent::StreamIdNotUtf8)
            .map(String::from)?;

        let data_bytes = &bytes[(FIXED_HEADER_LEN + stream_len as usize)..];
        let data = Vec::<u8>::from(data_bytes);

        Ok(Event {
            stream,
            version,
            data,
        })
    }
}

#[derive(Debug, PartialEq)]
pub enum MalformedEvent {
    TooShort,
    StreamIdLengthZero,
    StreamIdOutOfRange,
    StreamIdNotUtf8,
}

impl Display for MalformedEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MalformedEvent::TooShort => {
                write!(f, "frame shorter than its header")
            }
            MalformedEvent::StreamIdLengthZero => {
                write!(f, "stream id length is zero")
            }
            MalformedEvent::StreamIdOutOfRange => {
                write!(f, "stream id length past end of frame")
            }
            MalformedEvent::StreamIdNotUtf8 => {
                write!(f, "stream id not utf-8")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod encode {
        use crate::event::*;

        #[test]
        fn encodes_known_bytes() {
            let expected: Vec<u8> = vec![
                0x08, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6f, 0x72, 0x64, 0x65,
                0x72, 0x73, 0x2d, 0x31, 0x68, 0x69,
            ];
            let event = Event {
                stream: String::from("orders-1"),
                version: 3,
                data: b"hi".to_vec(),
            };

            let result = event.encode().unwrap();
            assert_eq!(result, expected);
        }

        #[test]
        fn encodes_an_event_with_no_data() {
            let expected: Vec<u8> = vec![
                0x08, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6f, 0x72, 0x64, 0x65,
                0x72, 0x73, 0x2d, 0x31,
            ];
            let event = Event {
                stream: String::from("orders-1"),
                version: 3,
                data: vec![],
            };

            let result = event.encode().unwrap();
            assert_eq!(result, expected);
        }

        #[test]
        fn rejects_an_empty_stream_id() {
            let event = Event {
                stream: String::from(""),
                version: 0,
                data: vec![],
            };

            let result = event.encode();

            assert!(matches!(result.unwrap_err(), StoreError::EmptyStreamId));
        }

        #[test]
        fn rejects_a_stream_id_longer_than_u16_max() {
            let event = Event {
                stream: "a".repeat(u16::MAX as usize + 1),
                version: 0,
                data: vec![],
            };

            let result = event.encode();

            assert!(matches!(
                result.unwrap_err(),
                StoreError::StreamIdTooLong { len } if len == u16::MAX as usize + 1
            ));
        }
    }

    mod decode {
        use crate::event::*;

        #[test]
        fn decodes_known_bytes() {
            let bytes: Vec<u8> = vec![
                0x08, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6f, 0x72, 0x64, 0x65,
                0x72, 0x73, 0x2d, 0x31, 0x68, 0x69,
            ];
            let expected = Event {
                stream: String::from("orders-1"),
                version: 3,
                data: b"hi".to_vec(),
            };

            let event = Event::decode(&bytes).unwrap();
            assert_eq!(event, expected);
        }

        #[test]
        fn decodes_a_frame_with_no_data() {
            let bytes: Vec<u8> = vec![
                0x08, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6f, 0x72, 0x64, 0x65,
                0x72, 0x73, 0x2d, 0x31,
            ];
            let expected = Event {
                stream: String::from("orders-1"),
                version: 3,
                data: vec![],
            };

            let event = Event::decode(&bytes).unwrap();
            assert_eq!(event, expected);
        }

        #[test]
        fn rejects_a_payload_shorter_than_the_header() {
            let bytes: Vec<u8> = vec![0x08, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
            let err = Event::decode(&bytes).unwrap_err();

            assert!(matches!(err, MalformedEvent::TooShort));
        }

        #[test]
        fn rejects_a_zero_length_stream_id() {
            let bytes: Vec<u8> = vec![0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
            let err = Event::decode(&bytes).unwrap_err();

            assert!(matches!(err, MalformedEvent::StreamIdLengthZero));
        }

        #[test]
        fn rejects_a_stream_length_past_the_end() {
            let bytes: Vec<u8> = vec![0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
            let err = Event::decode(&bytes).unwrap_err();

            assert!(matches!(err, MalformedEvent::StreamIdOutOfRange));
        }

        #[test]
        fn rejects_a_non_utf8_stream_id() {
            let bytes: Vec<u8> = vec![
                0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF,
            ];
            let err = Event::decode(&bytes).unwrap_err();

            assert!(matches!(err, MalformedEvent::StreamIdNotUtf8));
        }
    }

    mod round_trip {
        use crate::event::*;

        #[test]
        fn round_trips_an_event() {
            let event = Event {
                stream: String::from("orders-1"),
                version: 3,
                data: b"hi".to_vec(),
            };
            let bytes = event.encode().unwrap();
            let rebuilt = Event::decode(&bytes).unwrap();

            assert_eq!(rebuilt, event);
        }
    }
}
