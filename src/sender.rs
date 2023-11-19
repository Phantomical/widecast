use std::sync::atomic::Ordering;

use triomphe::Arc;

use crate::{ArcCache, Receiver, Shared};

pub struct Sender<T> {
    shared: Arc<Shared<T>>,
    cache: ArcCache<T>,
}

impl<T> Sender<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            shared: Arc::new(Shared::with_capacity(capacity)),
            cache: ArcCache::new(),
        }
    }

    pub fn subscribe(&self) -> Receiver<T> {
        Receiver::new(self.shared.clone())
    }

    pub fn send(&mut self, value: T) {
        self.shared.buffer.push(value, &mut self.cache);
        self.wake_receivers();
    }

    pub fn send_bulk<I: Iterator<Item = T>>(&mut self, iter: I) {
        self.shared.buffer.push_bulk(iter, &mut self.cache);
        self.wake_receivers();
    }

    pub fn receiver_count(&self) -> usize {
        Arc::count(&self.shared) - 1
    }

    fn wake_receivers(&self) {
        for queue in &self.shared.queues {
            queue.set_watermark();
            queue.wake_remote(8);
        }

        for queue in &self.shared.queues {
            queue.wake_remote(u64::MAX);
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.wake_receivers();
    }
}
