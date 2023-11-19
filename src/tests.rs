use assert_matches::assert_matches;

use crate::*;

#[test]
fn test_send_one() {
    let mut tx = Sender::new(1);
    let mut rx = tx.subscribe();

    tx.send(());
    assert_matches!(rx.try_recv().as_deref(), Ok(()));
}

#[test]
fn test_send_none() {
    let tx = Sender::<()>::new(1);
    let mut rx = tx.subscribe();

    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
}


#[test]
fn test_send_multiple() {
    let mut tx = Sender::new(128);
    let mut rx = tx.subscribe();

    tx.send(());
    tx.send(());
    tx.send(());
    tx.send(());

    assert_matches!(rx.try_recv().as_deref(), Ok(()));
}
