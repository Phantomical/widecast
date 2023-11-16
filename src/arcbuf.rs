use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::{ArcSwapOption, DefaultStrategy, Guard};

pub(crate) struct ArcBuffer<T> {
    values: Vec<ArcSwapOption<T>>,
    head: AtomicU64,
    tail: AtomicU64,
}

impl<T> ArcBuffer<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        let mut values = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            values.push(ArcSwapOption::new(None));
        }

        Self {
            values,
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
        }
    }
}

impl<T> ArcBuffer<T> {
    pub fn head(&self) -> u64 {
        self.head.load(Ordering::Relaxed)
    }

    /// Push a new value at the end of the ringbuffer.
    ///
    /// This may not be called concurrently.
    pub fn push(&self, value: Arc<T>) -> Option<Arc<T>> {
        BufferState::new(self).push(value)
    }

    /// Push a set of values in bulk to the end of the ringbuffer.
    pub fn push_bulk<I>(&self, iter: I)
    where
        I: Iterator<Item = Arc<T>>,
    {
        BufferState::new(self).push_bulk(iter);
    }

    /// Get the value contained within this ringbuffer at the given index.
    pub fn get(&self, index: u64) -> Result<Guard<Option<Arc<T>>, DefaultStrategy>, IndexError> {
        let idx = self.index(index);
        let head = self.head.load(Ordering::Acquire);
        if index > head {
            return Err(IndexError::Invalid);
        }

        // The acquire load for self.head above synchronizes with the write to
        // this value in self.push.
        let value = self.values[idx].load();

        // This needs to happen _after_ the read above. The only way to
        // guarantee that is to use the SeqCst ordering.
        let tail = self.tail.load(Ordering::SeqCst);
        if index < tail {
            return Err(IndexError::Outdated(tail));
        }

        Ok(value)
    }

    pub fn capacity(&self) -> usize {
        self.values.len()
    }

    fn index(&self, index: u64) -> usize {
        (index % self.values.len() as u64) as usize
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) enum IndexError {
    /// The index is less than the current tail index.
    ///
    /// Contains the current tail index.
    Outdated(u64),

    /// The index was greater than the current head index.
    Invalid,
}

struct BufferState<'b, T> {
    buffer: &'b ArcBuffer<T>,

    head: u64,
    tail: u64,
}

impl<'b, T> BufferState<'b, T> {
    /// Load the `BufferState` from the buffer.
    ///
    /// # Safety
    /// This may not be called from multiple threads concurrently.
    pub fn new(buffer: &'b ArcBuffer<T>) -> Self {
        let head = buffer.head.load(Ordering::Relaxed);
        let tail = buffer.tail.load(Ordering::Relaxed);

        Self { buffer, head, tail }
    }

    /// Advance tail to reserve space for `count` additional elements.
    pub fn reserve(&mut self, count: u64) {
        let spare = self.buffer.capacity() as u64 - (self.head - self.tail);

        if let Some(advance) = count.checked_sub(spare) {
            self.tail += advance;
            self.buffer.tail.store(self.tail, Ordering::SeqCst);
        }
    }

    /// Advance head to make `count` new elements visible to readers.
    pub fn advance(&mut self, count: u64) {
        self.head += count;
        self.buffer.head.store(self.head, Ordering::Release);
    }

    pub fn index(&self, index: u64) -> usize {
        (index % self.buffer.capacity() as u64) as usize
    }

    pub fn swap_raw(&self, index: usize, value: Arc<T>) -> Option<Arc<T>> {
        self.buffer.values[index].swap(Some(value))
    }

    pub fn push(&mut self, value: Arc<T>) -> Option<Arc<T>> {
        self.reserve(1);

        let index = self.index(self.head + 1);
        let prev = self.swap_raw(index, value);

        self.advance(1);

        prev
    }

    pub fn push_bulk<I>(&mut self, mut iter: I)
    where
        I: Iterator<Item = Arc<T>>,
    {
        let estimate = iter.size_hint().0 as u64;
        let saved_tail = self.tail;

        let mut prev = None;
        let mut index = self.index(self.head + 1);
        self.reserve(estimate);

        for offset in 0..estimate {
            // Explicitly drop prev within the scopeguard so that if the drop
            // panics then we restore the buffer to a valid state.
            let _ = prev.take();

            // In case of panic or early exit we need to fix up indices.
            //
            // Since we are moving the tail backwards this may cause readers to
            // skip values that, strictly speaking, they may have been able to
            // see. This is not really an issue since broadcast streams allow
            // for missed value and since it only happens if the source iterator
            // is implemented incorrectly.
            let guard = scopeguard::guard((), |_| {
                self.tail = saved_tail;
                self.reserve(offset);
                self.advance(offset);
            });

            let value = match iter.next() {
                Some(value) => value,
                None => return,
            };

            std::mem::forget(guard);

            prev = self.swap_raw(index, value);
            index += 1;
            if index >= self.buffer.capacity() {
                index -= self.buffer.capacity();
            }
        }

        self.advance(estimate);
        drop(prev);

        while let Some(value) = iter.next() {
            self.push(value);
        }
    }
}
