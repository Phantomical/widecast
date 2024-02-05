//! The underlying raw queue that is used to implement the channel.

use std::sync::atomic::Ordering;

use self::chunk::Chunk;
use crate::shim::{Arc, ArcSwap, AtomicU64};

mod chunk;
mod reader;
mod writer;

pub use self::reader::Drain;
pub(crate) use self::reader::Reader;
pub(crate) use self::writer::Writer;

pub(crate) struct Shared<T> {
    /// A pointer to the current buffer backing this queue.
    chunk: ArcSwap<Chunk<T>>,

    /// The head index for the ringbuffer.
    ///
    /// When a new value is inserted into the ringbuffer we increment this
    /// index.
    head: AtomicU64,

    /// The tail index for the ringbuffer.
    ///
    /// This counter points to the last value that is logically _contained_
    /// within the ringbuffer.
    tail: AtomicU64,

    /// The remote tail for the ringbuffer.
    ///
    /// This counter points to the last value that is still potentially being
    /// read from the ringbuffer.
    remote: AtomicU64,
}

impl<T> Shared<T> {
    pub fn new(capacity: usize) -> Self {
        assert_ne!(capacity, 0);

        let chunk = Chunk::new(capacity.checked_next_power_of_two().unwrap_or(capacity));

        Self {
            chunk: ArcSwap::new(Arc::new(chunk)),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            remote: AtomicU64::new(0),
        }
    }

    pub fn head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    pub fn tail(&self) -> u64 {
        self.tail.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.head() == self.tail()
    }

    fn update_tail(&self, tail: u64) {
        self.tail.store(tail, Ordering::Release);
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        if !std::mem::needs_drop::<T>() {
            return;
        }

        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        let data = self.chunk.load();

        let (range1, range2) = data.translate_range(tail..head);

        // SAFETY
        // ------
        // This safety block applies to both unsafe blocks below.
        // Addressing the safety requirements one-by-one:
        //
        // # All values in the range must be live values.
        //
        // `tail` and `head` demarcate the region in which valid values are stored in
        // the buffer. The translated ranges are thereforce guaranteed to
        // contain live values.
        //
        // # There must be no concurrent accesses to the same region.
        //
        // All methods which write to the range marked by `head` and `tail`
        // require a shared reference to the `Shared<T>` instance. Since we have a
        // mutable reference, there is nobody else writing to the live region of the
        // buffer.

        unsafe { data.drop_range(range1) };
        if let Some(range2) = range2 {
            unsafe { data.drop_range(range2) };
        }
    }
}
