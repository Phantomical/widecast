//! Direct and unsafe access to a ringbuffer queue.

use crate::detail::{Reader, Shared, Writer};
use crate::shim::UnsafeCell;
use crate::Drain;

pub struct RawChannel<T> {
    shared: Shared<T>,
    reader: UnsafeCell<Reader<T>>,
    writer: UnsafeCell<Writer<T>>,
}

impl<T> Default for RawChannel<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> RawChannel<T> {
    /// Create a new queue with a default capacity.
    pub fn new() -> Self {
        Self::with_capacity(16)
    }

    /// Create a new queue with the requested initial capacity.
    ///
    /// # Panics
    /// - This will panic if `capacity` is 0.
    pub fn with_capacity(capacity: usize) -> Self {
        let shared = Shared::new(capacity);

        Self {
            reader: UnsafeCell::new(Reader::new(&shared)),
            writer: UnsafeCell::new(Writer::new(&shared)),
            shared,
        }
    }

    /// Get the current head index of the queue.
    ///
    /// This is the index at which the next value will be inserted into the
    /// queue.
    pub fn head(&self) -> u64 {
        self.shared.head()
    }

    /// Get the current tail index of the queue.
    ///
    /// This is the index of the next value to be removed from the queue.
    pub fn tail(&self) -> u64 {
        self.shared.tail()
    }

    /// Push a new value onto this queue.
    ///
    /// This method is a **writer** method.
    ///
    /// Returns the queue index at which `value` was inserted.
    ///
    /// # Safety
    /// - This method may not be called concurrently with any other **writer**
    ///   methods.
    pub unsafe fn push(&self, value: T) -> u64 {
        self.writer.with_mut(|writer| {
            // SAFETY: The contract for this function guarantees that this is correct.
            let writer = unsafe { &mut *writer };

            writer.push(&self.shared, value)
        })
    }

    /// Pop a value from this queue.
    ///
    /// This method is a **reader** method.
    ///
    /// # Safety
    /// - This method may not be called concurrently with any other **reader**
    ///   methods.
    pub unsafe fn pop(&self) -> Option<T> {
        self.reader.with_mut(|reader| {
            // SAFETY: The contract for this function guarantees that this is correct.
            let reader = unsafe { &mut *reader };

            reader.pop(&self.shared)
        })
    }

    /// Drain all values within the queue via an iterator.
    ///
    /// This method is a **reader** method.
    ///
    /// # Safety
    /// - This method may not be called concurrently with any other **reader**
    ///   methods.
    /// - Furthermore, no **reader** methods may be called while the [`Drain`]
    ///   returned by this function is still live. Calling them once the
    ///   [`Drain`] has been dropped or forgotten is fine, however.
    pub unsafe fn drain(&self) -> Drain<T> {
        unsafe { self.drain_to(u64::MAX) }
    }

    /// Drain all values up to a queue index via an iterator.
    ///
    /// This method is a **reader** method.
    ///
    /// There are no restrictions on the value of `watermark`. If it greater
    /// than the current queue head then the whole queue will be drained. If it
    /// is smaller then no elements will be drained.
    ///
    /// # Safety
    /// - This method may not be called concurrently with any other **reader**
    ///   methods.
    /// - Furthermore, no **reader** methods may be called while the [`Drain`]
    ///   returned by this function is still live. Calling them once the
    ///   [`Drain`] has been dropped or forgotten is fine, however.
    pub unsafe fn drain_to(&self, watermark: u64) -> Drain<T> {
        let drain = self.reader.with_mut(|reader| {
            let reader = unsafe { &mut *reader };

            reader.consume(&self.shared, watermark)
        });

        #[cfg(loom)]
        let drain = drain.with_loom_ptr(self.reader.get_mut());

        drain
    }
}
