use foundry_network::{
    NetworkError,
    ipc_codec::{MAX_IPC_BYTES, read_record, read_reply},
};
use std::time::Duration;
use tokio::io::BufReader;

#[tokio::test]
async fn explicit_reply_reader_accepts_large_record_without_enlarging_requests() {
    let expected = "q".repeat(2 * MAX_IPC_BYTES);
    let mut bytes = serde_json::to_vec(&expected).unwrap();
    bytes.extend_from_slice(b"\n7\n");
    assert!(matches!(
        read_record::<String>(
            &mut BufReader::new(bytes.as_slice()),
            Duration::from_secs(2)
        )
        .await,
        Err(NetworkError::Invalid(_))
    ));
    let mut reader = BufReader::new(bytes.as_slice());
    assert_eq!(
        read_reply::<String>(&mut reader, Duration::from_secs(2))
            .await
            .unwrap(),
        Some(expected)
    );
    assert_eq!(
        read_reply::<u8>(&mut reader, Duration::from_secs(2))
            .await
            .unwrap(),
        Some(7)
    );
    assert!(
        read_reply::<u8>(&mut reader, Duration::from_secs(2))
            .await
            .unwrap()
            .is_none()
    );
}
