use std::sync::atomic::Ordering;

use super::chunk::Chunk;
use super::Shared;
use crate::shim::Arc;

pub(crate) struct Writer<T> {
    chunk: Arc<Chunk<T>>,
}

impl<T> Writer<T> {
    pub fn new(shared: &Shared<T>) -> Self {
        Self {
            chunk: shared.chunk.load_full(),
        }
    }

    pub fn push(&mut self, shared: &Shared<T>, value: T) -> u64 {
        let head = shared.head.load(Ordering::Acquire);
        let tail = shared.remote.load(Ordering::Acquire);

        if tail + self.chunk.capacity() as u64 == head {
            self.expand(shared);
        }

        let slot = self.chunk.get_logical(head);

        // SAFETY: This chunk is outside of the range that is permitted to be accessed
        //         by the reader (remote to head, not including head) so writing to it
        //         is safe.
        slot.with_mut(|slot| unsafe { (*slot).write(value) });

        shared.head.store(head + 1, Ordering::Release);
        head
    }

    fn expand(&mut self, shared: &Shared<T>) {
        let cap = self.chunk.capacity();

        let head = shared.head.load(Ordering::Acquire);
        let tail = shared.tail.load(Ordering::Acquire);

        let mut chunk = Chunk::new(cap * 2);

        // SAFETY: Local<T> is the only code that is allowed to write to the shared
        //         buffer. Therefore we can guarantee that there are no concurrent
        //         writes.
        unsafe { chunk.copy_from(&self.chunk, tail..head) };

        let chunk = Arc::new(chunk);
        self.chunk = chunk.clone();
        shared.chunk.store(chunk);
    }
}
