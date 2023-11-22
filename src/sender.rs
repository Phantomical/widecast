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
        self.shared.notify.notify_all();
    }

    pub fn send_bulk<I: Iterator<Item = T>>(&mut self, iter: I) {
        self.shared.buffer.push_bulk(iter, &mut self.cache);
        self.shared.notify.notify_all();
    }

    pub fn receiver_count(&self) -> usize {
        Arc::count(&self.shared) - 1
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.notify.notify_all();
    }
}
