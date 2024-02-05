use widecast_spsc::channel;

struct NeedsDrop;

impl Drop for NeedsDrop {
    fn drop(&mut self) {}
}

#[test]
fn test_send_and_recv() {
    let (mut tx, mut rx) = channel(4);

    tx.send(5u32).unwrap();
    assert_eq!(rx.recv().unwrap(), 5);
}

#[test]
fn test_send_and_drain() {
    let (mut tx, mut rx) = channel::<u32>(4);

    tx.send(1).unwrap();
    tx.send(2).unwrap();
    tx.send(3).unwrap();
    tx.send(4).unwrap();

    let vals: Vec<_> = rx.drain().unwrap().collect();
    assert_eq!(vals, vec![1, 2, 3, 4]);
}
