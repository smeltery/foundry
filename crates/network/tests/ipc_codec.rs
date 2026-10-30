use foundry_network::{NetworkError, ipc_codec::*};
use serde_json::{Value, json};
use std::{io, time::Duration};
use tokio::io::{AsyncWriteExt, BufReader, duplex};

const DEADLINE: Duration = Duration::from_secs(2);

#[tokio::test]
async fn fragmented_records_preserve_boundaries_and_clean_eof() {
    let (mut sender, receiver) = duplex(1);
    let producer = tokio::spawn(async move {
        for byte in b"{\"id\":1}\n{\"id\":2}\r\n" {
            sender.write_all(&[*byte]).await.unwrap();
        }
    });
    let mut reader = BufReader::with_capacity(3, receiver);
    for id in [1, 2] {
        assert_eq!(
            read_record::<Value>(&mut reader, DEADLINE).await.unwrap(),
            Some(json!({"id":id}))
        );
    }
    assert_eq!(
        read_record::<Value>(&mut reader, DEADLINE).await.unwrap(),
        None
    );
    producer.await.unwrap();
}

#[tokio::test]
async fn malformed_and_unterminated_records_fail() {
    for bytes in [b"\n".as_slice(), b"{\n", b"\xff\n"] {
        assert!(matches!(
            read_record::<Value>(&mut BufReader::new(bytes), DEADLINE).await,
            Err(NetworkError::Json(_))
        ));
    }
    assert!(
        matches!(read_record::<Value>(&mut BufReader::new(b"{}".as_slice()), DEADLINE).await,
        Err(NetworkError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof)
    );
}

#[tokio::test]
async fn exact_limit_roundtrips_and_oversized_output_writes_nothing() {
    let value = "x".repeat(MAX_IPC_BYTES - 2);
    let mut bytes = Vec::new();
    write_record(&mut bytes, &value, DEADLINE).await.unwrap();
    assert_eq!(bytes.len(), MAX_IPC_BYTES + 1);
    assert_eq!(
        read_record::<String>(&mut BufReader::new(bytes.as_slice()), DEADLINE)
            .await
            .unwrap(),
        Some(value.clone())
    );
    bytes.clear();
    assert!(
        write_record(&mut bytes, &(value + "x"), DEADLINE)
            .await
            .is_err()
    );
    assert!(bytes.is_empty());
    let oversized = vec![b'x'; MAX_IPC_BYTES + 1];
    assert!(matches!(
        read_record::<Value>(&mut BufReader::new(oversized.as_slice()), DEADLINE).await,
        Err(NetworkError::Invalid(_))
    ));
}

#[tokio::test]
async fn deadlines_cover_idle_reads_and_slow_writes() {
    let (mut sender, receiver) = duplex(1);
    let mut reader = BufReader::new(receiver);
    assert!(matches!(
        read_record::<Value>(&mut reader, Duration::from_millis(10)).await,
        Err(NetworkError::TimedOut)
    ));
    assert!(matches!(
        write_record(&mut sender, &json!({"id":1}), Duration::from_millis(10)).await,
        Err(NetworkError::TimedOut)
    ));
    assert!(matches!(
        read_record::<Value>(&mut reader, Duration::ZERO).await,
        Err(NetworkError::Invalid(_))
    ));
    assert!(matches!(
        write_record(&mut Vec::new(), &0, Duration::ZERO).await,
        Err(NetworkError::Invalid(_))
    ));
    drop(reader);
    assert!(matches!(
        write_record(&mut sender, &0, DEADLINE).await,
        Err(NetworkError::Io(_))
    ));
}
