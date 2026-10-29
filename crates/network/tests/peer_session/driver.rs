use super::*;

async fn observe(driver: &mut PeerDriver, received: &str, queued: &str) {
    let mut receipt = false;
    let mut incoming = false;
    for _ in 0..2 {
        match tokio::time::timeout(WAIT, driver.next_event())
            .await
            .unwrap()
            .unwrap()
        {
            DriverEvent::Peer(PeerEvent::Received { message_id, .. }) => {
                assert_eq!(message_id, received);
                incoming = true;
            }
            DriverEvent::Peer(PeerEvent::Queued { message_id }) => {
                assert_eq!(message_id, queued);
                receipt = true;
            }
            _ => panic!("unexpected driver event"),
        }
    }
    assert!(receipt && incoming);
}

#[tokio::test]
async fn persistent_driver_reads_while_both_peers_send_and_joins_on_shutdown() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (sender, receiver) =
        pair(&left, &right, a.authority(true), b.authority(true), &cancel).await;
    let (send_a, mut drive_a) = PeerDriver::start(sender).unwrap();
    let (send_b, mut drive_b) = PeerDriver::start(receiver).unwrap();
    let outward = message();
    let mut returning = message();
    returning.id = "returning-1".into();
    std::mem::swap(&mut returning.from, &mut returning.to);
    let (a_sent, b_sent) = tokio::join!(send_a.send_text(outward), send_b.send_text(returning));
    a_sent.unwrap();
    b_sent.unwrap();
    tokio::join!(
        observe(&mut drive_a, "returning-1", "receipt-1"),
        observe(&mut drive_b, "receipt-1", "returning-1")
    );
    assert_eq!(
        mail_store::MailStore::open(&a.root)
            .unwrap()
            .unread("alice/desktop/terminal")
            .unwrap(),
        1
    );
    assert_eq!(
        mail_store::MailStore::open(&b.root)
            .unwrap()
            .unread("bob/desktop/terminal")
            .unwrap(),
        1
    );
    let (a_done, b_done) = tokio::join!(drive_a.shutdown(), drive_b.shutdown());
    a_done.unwrap();
    b_done.unwrap();
    assert_eq!(send_a.publish_state().await, Err(PeerError::Closed));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn receive_only_driver_survives_dropping_outbound_handle() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (sender, receiver) = pair(
        &left,
        &right,
        a.authority(false),
        b.authority(true),
        &cancel,
    )
    .await;
    let (send_a, mut drive_a) = PeerDriver::start(sender).unwrap();
    let (send_b, mut drive_b) = PeerDriver::start(receiver).unwrap();
    drop(send_b);
    send_a.send_text(message()).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(WAIT, drive_b.next_event())
            .await
            .unwrap(),
        Some(DriverEvent::Peer(PeerEvent::Received { .. }))
    ));
    assert!(matches!(
        tokio::time::timeout(WAIT, drive_a.next_event())
            .await
            .unwrap(),
        Some(DriverEvent::Peer(PeerEvent::Queued { .. }))
    ));
    let (a_done, b_done) = tokio::join!(drive_a.shutdown(), drive_b.shutdown());
    a_done.unwrap();
    b_done.unwrap();
    tokio::join!(left.shutdown(), right.shutdown());
}
