mod common;

use common::*;
use lr_abstractions::{Frame, TransportError, MAX_FRAME_BYTES};
use lr_net::codec::{read_frame, read_frame_limited, write_end, write_frame, END_OF_STREAM};
use lr_proto::FrameError;
use tokio::io::{duplex, AsyncWriteExt};

#[tokio::test]
async fn frames_round_trip_in_order() {
    let (mut a, mut b) = duplex(1 << 16);
    for msg in [&b"{}"[..], b"{\"type\":\"heartbeat\"}", b""] {
        write_frame(&mut a, msg).await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), msg);
    }
}

#[tokio::test]
async fn frame_of_exactly_4_mib_accepted() {
    let (mut a, mut b) = duplex(1 << 20);
    let body = vec![b'x'; MAX_FRAME_BYTES];
    let writer = tokio::spawn(async move {
        write_frame(&mut a, &body).await.unwrap();
    });
    assert_eq!(read_frame(&mut b).await.unwrap().len(), MAX_FRAME_BYTES);
    writer.await.unwrap();
}

#[tokio::test]
async fn declared_4_mib_plus_one_refused_before_the_body() {
    let (mut a, mut b) = duplex(64);
    a.write_all(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes())
        .await
        .unwrap();
    let read = tokio::time::timeout(std::time::Duration::from_secs(2), read_frame(&mut b))
        .await
        .expect("refused at once, without waiting for the body");
    assert_eq!(
        read,
        Err(FrameError::TooLarge {
            len: MAX_FRAME_BYTES as u64 + 1
        })
    );
    a.write_all(&(u32::MAX - 1).to_be_bytes()).await.unwrap();
    assert!(matches!(
        read_frame(&mut b).await,
        Err(FrameError::TooLarge { .. })
    ));
}

#[tokio::test]
async fn writing_over_4_mib_refused_and_nothing_written() {
    let (mut a, mut b) = duplex(1 << 16);
    let body = vec![b'x'; MAX_FRAME_BYTES + 1];
    assert!(matches!(
        write_frame(&mut a, &body).await,
        Err(FrameError::TooLarge { .. })
    ));
    write_frame(&mut a, b"{}").await.unwrap();
    assert_eq!(read_frame(&mut b).await.unwrap(), b"{}");
}

#[tokio::test]
async fn truncated_frames_reported() {
    let (mut a, mut b) = duplex(64);
    a.write_all(&10u32.to_be_bytes()).await.unwrap();
    a.write_all(b"12345").await.unwrap();
    drop(a);
    assert_eq!(read_frame(&mut b).await, Err(FrameError::Truncated));

    let (mut a, mut b) = duplex(64);
    a.write_all(&[0, 0]).await.unwrap();
    drop(a);
    assert_eq!(read_frame(&mut b).await, Err(FrameError::Truncated));

    let (a, mut b) = duplex(64);
    drop(a);
    assert_eq!(read_frame(&mut b).await, Err(FrameError::Closed));
}

#[tokio::test]
async fn split_prefix_and_body_are_reassembled() {
    let (mut a, mut b) = duplex(64);
    let reader = tokio::spawn(async move { read_frame(&mut b).await });
    for byte in [0u8, 0, 0, 3, b'a', b'b', b'c'] {
        a.write_all(&[byte]).await.unwrap();
        a.flush().await.unwrap();
        tokio::task::yield_now().await;
    }
    assert_eq!(reader.await.unwrap().unwrap(), b"abc");
}

#[tokio::test]
async fn lower_limit_applies_before_authentication() {
    let (mut a, mut b) = duplex(64);
    a.write_all(&5000u32.to_be_bytes()).await.unwrap();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        read_frame_limited(&mut b, 4096),
    )
    .await
    .expect("refused at once, without waiting for the body");
    assert_eq!(read, Err(FrameError::TooLarge { len: 5000 }));
}

#[tokio::test]
async fn transport_refuses_oversized_send_and_stays_usable() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let client = b.dial(&k).await.unwrap();
    let server = b.accept().await;
    let big = Frame::from_bytes(vec![b'x'; MAX_FRAME_BYTES + 1]);
    assert_eq!(
        client.send(big).await,
        Err(TransportError::TooLarge {
            len: MAX_FRAME_BYTES + 1
        })
    );
    exchange(client.as_ref(), server.as_ref()).await;
    let max = Frame::from_bytes(vec![b'y'; MAX_FRAME_BYTES]);
    let sender = tokio::spawn(async move {
        client.send(max).await.unwrap();
        client
    });
    assert_eq!(server.recv().await.unwrap().len(), MAX_FRAME_BYTES);
    sender.await.unwrap();
}

#[tokio::test]
async fn end_marker_closes_the_stream_cleanly() {
    let (mut a, mut b) = duplex(64);
    write_frame(&mut a, b"{}").await.unwrap();
    write_end(&mut a).await.unwrap();
    assert_eq!(read_frame(&mut b).await.unwrap(), b"{}");
    assert_eq!(read_frame(&mut b).await, Err(FrameError::Closed));
    assert!(END_OF_STREAM as usize > MAX_FRAME_BYTES);
}
