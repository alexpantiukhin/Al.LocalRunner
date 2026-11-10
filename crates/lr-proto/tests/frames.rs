mod common;

use common::*;
use lr_core::MAX_FRAME_BYTES;
use lr_proto::*;
use std::io::{Cursor, Read};

struct Guarded {
    data: Cursor<Vec<u8>>,
    max_read: usize,
    read: usize,
}

impl Read for Guarded {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.data.read(buf)?;
        self.read += n;
        assert!(
            self.read <= self.max_read,
            "reader consumed {} bytes, allowed {}",
            self.read,
            self.max_read
        );
        Ok(n)
    }
}

#[test]
fn frame_limit_is_four_mib_from_core() {
    assert_eq!(FRAME_LIMIT, 4 * 1024 * 1024);
    assert_eq!(FRAME_LIMIT, MAX_FRAME_BYTES);
}

#[test]
fn frame_over_4mib_rejected_both_ways() {
    let big = vec![b'x'; MAX_FRAME_BYTES + 1];
    let mut out = Vec::new();
    let err = write_frame(&mut out, &big).unwrap_err();
    assert_eq!(
        err,
        FrameError::TooLarge {
            len: (MAX_FRAME_BYTES + 1) as u64
        }
    );
    assert!(out.is_empty(), "nothing is written for a refused frame");

    let mut wire = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec();
    wire.extend(std::iter::repeat_n(b'x', MAX_FRAME_BYTES + 1));
    let err = read_frame(&mut Cursor::new(wire)).unwrap_err();
    assert_eq!(
        err,
        FrameError::TooLarge {
            len: (MAX_FRAME_BYTES + 1) as u64
        }
    );
}

#[test]
fn reader_checks_length_prefix_before_reading_the_body() {
    let mut wire = u32::MAX.to_be_bytes().to_vec();
    wire.extend_from_slice(&[0u8; 64]);
    let mut r = Guarded {
        data: Cursor::new(wire),
        max_read: 4,
        read: 0,
    };
    assert_eq!(
        read_frame(&mut r).unwrap_err(),
        FrameError::TooLarge {
            len: u32::MAX as u64
        }
    );
}

#[test]
fn frame_of_exactly_4mib_passes() {
    let payload = vec![b'y'; MAX_FRAME_BYTES];
    let mut wire = Vec::new();
    write_frame(&mut wire, &payload).unwrap();
    assert_eq!(wire.len(), MAX_FRAME_BYTES + 4);
    assert_eq!(read_frame(&mut Cursor::new(wire)).unwrap(), payload);
}

#[test]
fn frames_follow_one_another_and_end_cleanly() {
    let mut wire = Vec::new();
    write_frame(&mut wire, b"one").unwrap();
    write_frame(&mut wire, b"").unwrap();
    write_frame(&mut wire, b"three").unwrap();
    let mut r = Cursor::new(wire);
    assert_eq!(read_frame(&mut r).unwrap(), b"one");
    assert_eq!(read_frame(&mut r).unwrap(), b"");
    assert_eq!(read_frame(&mut r).unwrap(), b"three");
    assert_eq!(read_frame(&mut r).unwrap_err(), FrameError::Closed);
}

#[test]
fn prefix_is_four_bytes_big_endian() {
    let mut wire = Vec::new();
    write_frame(&mut wire, b"abc").unwrap();
    assert_eq!(wire, [0, 0, 0, 3, b'a', b'b', b'c']);
    assert_eq!(encode_frame(b"abc").unwrap(), wire);
    assert_eq!(parse_prefix([0, 0, 1, 0]).unwrap(), 256);
    assert!(parse_prefix((MAX_FRAME_BYTES as u32 + 1).to_be_bytes()).is_err());
}

#[test]
fn truncated_frames_are_reported() {
    let mut wire = Vec::new();
    write_frame(&mut wire, b"hello world").unwrap();
    wire.truncate(wire.len() - 3);
    assert_eq!(
        read_frame(&mut Cursor::new(wire)).unwrap_err(),
        FrameError::Truncated
    );
    assert_eq!(
        read_frame(&mut Cursor::new(vec![0, 0])).unwrap_err(),
        FrameError::Truncated
    );
}

#[test]
fn encoding_a_message_over_the_limit_is_refused() {
    let msg = RunnerToBroker::Chunk(Chunk {
        job: job(1),
        seq: 1,
        step: None,
        text: "z".repeat(MAX_FRAME_BYTES),
    });
    assert!(matches!(
        encode(&msg).unwrap_err(),
        FrameError::TooLarge { .. }
    ));
}

#[test]
fn decoding_a_document_over_the_limit_is_refused_without_parsing() {
    let bytes = vec![b' '; MAX_FRAME_BYTES + 1];
    assert!(matches!(
        decode::<RunnerToBroker>(&bytes).unwrap_err(),
        DecodeError::TooLarge { .. }
    ));
}

#[test]
fn message_travels_through_a_frame() {
    let msg = HubToBroker::Heartbeat;
    let mut wire = Vec::new();
    write_frame(&mut wire, &encode(&msg).unwrap()).unwrap();
    let bytes = read_frame(&mut Cursor::new(wire)).unwrap();
    assert_eq!(decode::<HubToBroker>(&bytes).unwrap(), msg);
}
