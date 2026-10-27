use foundry_network::{MAX_FRAME_BYTES, NetworkError, PeerFrame, read_frame, write_frame};
use serde_json::json;
use std::{io, time::Duration};
use tokio::io::{AsyncWriteExt, duplex};

const DEADLINE: Duration = Duration::from_secs(2);
fn hello() -> PeerFrame {
    PeerFrame::Hello {
        team: "fixture".into(),
        proto: 1,
    }
}
fn packet(payload: &[u8]) -> Vec<u8> {
    let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

#[tokio::test]
async fn net_own_003_fragmented_prefix_body_multiple_frames_and_clean_eof() {
    let payload = serde_json::to_vec(&hello()).unwrap();
    let bytes = [packet(&payload), packet(&payload)].concat();
    let (mut sender, mut receiver) = duplex(1);
    let producer = tokio::spawn(async move {
        for byte in bytes {
            sender.write_all(&[byte]).await.unwrap();
        }
        sender.shutdown().await.unwrap();
    });
    for _ in 0..2 {
        assert_eq!(
            read_frame(&mut receiver, DEADLINE).await.unwrap(),
            Some(hello())
        );
    }
    assert!(read_frame(&mut receiver, DEADLINE).await.unwrap().is_none());
    producer.await.unwrap();
}

#[tokio::test]
async fn prefix_limits_are_rejected_without_waiting_for_body() {
    for length in [0, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
        let bytes = length.to_be_bytes();
        assert!(matches!(
            read_frame(&mut bytes.as_slice(), DEADLINE).await,
            Err(NetworkError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn malformed_tag_json_utf8_and_truncated_frames_fail_explicitly() {
    for payload in [
        b"{".as_slice(),
        br#"{"t":"unknown"}"#,
        &[0xff],
        br#"{"t":"hello","team":"x"}"#,
    ] {
        assert!(matches!(
            read_frame(&mut packet(payload).as_slice(), DEADLINE).await,
            Err(NetworkError::Json(_))
        ));
    }
    for bytes in [vec![0, 0], vec![0, 0, 0, 5, b'{']] {
        let error = read_frame(&mut bytes.as_slice(), DEADLINE)
            .await
            .unwrap_err();
        assert!(
            matches!(error, NetworkError::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
    }
}

#[tokio::test]
async fn exact_limit_roundtrips_and_oversized_serialization_writes_nothing() {
    let overhead = serde_json::to_vec(&PeerFrame::Hello {
        team: String::new(),
        proto: 1,
    })
    .unwrap()
    .len();
    let mut frame = PeerFrame::Hello {
        team: "x".repeat(MAX_FRAME_BYTES - overhead),
        proto: 1,
    };
    let mut output = Vec::new();
    write_frame(&mut output, &frame, DEADLINE).await.unwrap();
    assert_eq!(output.len(), MAX_FRAME_BYTES + 4);
    assert_eq!(
        read_frame(&mut output.as_slice(), DEADLINE).await.unwrap(),
        Some(frame.clone())
    );
    if let PeerFrame::Hello { team, .. } = &mut frame {
        team.push('x');
    }
    output.clear();
    assert!(write_frame(&mut output, &frame, DEADLINE).await.is_err());
    assert!(output.is_empty());
}

#[tokio::test]
async fn read_and_backpressured_write_have_deadlines_and_broken_pipe_propagates() {
    let (mut sender, mut receiver) = duplex(1);
    assert!(matches!(
        read_frame(&mut receiver, Duration::from_millis(10)).await,
        Err(NetworkError::TimedOut)
    ));
    assert!(matches!(
        write_frame(&mut sender, &hello(), Duration::from_millis(10)).await,
        Err(NetworkError::TimedOut)
    ));
    drop(receiver);
    assert!(matches!(
        write_frame(&mut sender, &hello(), DEADLINE).await,
        Err(NetworkError::Io(_))
    ));
    assert!(matches!(
        read_frame(&mut [].as_slice(), Duration::ZERO).await,
        Err(NetworkError::Invalid(_))
    ));
    assert!(matches!(
        write_frame(&mut Vec::new(), &hello(), Duration::ZERO).await,
        Err(NetworkError::Invalid(_))
    ));
}

#[tokio::test]
async fn optional_presence_fields_default_and_unknown_fields_remain_compatible() {
    let payload = json!({"t":"presence","presence":{"sessions":[],"known":[]},"future":true});
    let bytes = packet(&serde_json::to_vec(&payload).unwrap());
    let frame = read_frame(&mut bytes.as_slice(), DEADLINE)
        .await
        .unwrap()
        .unwrap();
    let value = serde_json::to_value(frame).unwrap();
    assert!(value["presence"].get("kept").is_none());
    assert!(value["presence"].get("features").is_none());
}
