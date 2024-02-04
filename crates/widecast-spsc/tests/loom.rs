#![cfg(loom)]

use std::sync::Arc;

use loom::thread;
use widecast_spsc::raw::RawQueue;

#[test]
fn concurrent_send_and_drain() {
    loom::model(|| {
        let v1 = Arc::new(RawQueue::<usize>::with_capacity(8));
        let v2 = v1.clone();

        thread::spawn(move || {
            for i in 0..32 {
                unsafe { v1.push(i) };
            }
        });

        let mut recvd = Vec::new();
        recvd.extend(unsafe { v2.drain() });

        // No extraneous values received.
        assert!(recvd.len() <= 32);
        // Values received in-order.
        assert!(recvd.iter().enumerate().all(|(idx, &val)| idx == val));
    })
}

#[test]
fn concurrent_send_and_drain_multiple() {
    loom::model(|| {
        let v1 = Arc::new(RawQueue::<usize>::with_capacity(1));
        let v2 = v1.clone();

        thread::spawn(move || {
            for i in 0..3 {
                unsafe { v1.push(i) };
            }
        });

        let mut recvd = Vec::new();
        recvd.extend(unsafe { v2.drain() });
        recvd.extend(unsafe { v2.drain() });

        // No extraneous values received.
        assert!(recvd.len() <= 32);
        // Values received in-order.
        assert!(recvd.iter().enumerate().all(|(idx, &val)| idx == val), "{recvd:?}");
    })
}

#[test]
fn concurrent_send_and_drain_drop() {
    loom::model(|| {
        let v1 = Arc::new(RawQueue::<usize>::with_capacity(8));
        let v2 = v1.clone();

        thread::spawn(move || {
            for i in 0..32 {
                unsafe { v1.push(i) };
            }
        });

        unsafe { v2.drain() };
    })
}

#[test]
fn concurrent_send_and_recv() {
    const COUNT: usize = 3;

    loom::model(|| {
        let v1 = Arc::new(RawQueue::<usize>::with_capacity(2));
        let v2 = v1.clone();

        thread::spawn(move || {
            for i in 0..COUNT {
                unsafe { v1.push(i) };
            }
        });

        let mut recvd = Vec::new();
        for _ in 0..COUNT {
            if let Some(item) = unsafe { v2.pop() } {
                recvd.push(item);
            }
        }

        // No extraneous values received.
        assert!(recvd.len() <= COUNT);
        // Values received in-order.
        assert!(recvd.iter().enumerate().all(|(idx, &val)| idx == val));
    })
}
