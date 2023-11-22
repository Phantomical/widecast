use std::cell::UnsafeCell;
use std::iter::Chain;
use std::mem::MaybeUninit;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::{ArcSwap, Guard};

/// Variant on a SPSC queue that allows the producer to also remove entries.
pub struct LocalQueue<T> {
    shared: Shared<T>,
    local: UnsafeCell<Local<T>>,
}

impl<T> LocalQueue<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        assert_ne!(capacity, 0);

        let chunk = Arc::new(Chunk::new(capacity));

        Self {
            shared: Shared {
                chunk: ArcSwap::new(chunk.clone()),
                head: AtomicU64::new(0),
                tail: AtomicU64::new(0),
                remote: AtomicU64::new(u64::MAX),
            },
            local: UnsafeCell::new(Local { chunk }),
        }
    }

    /// Fetch a watermark for use with `consume_local` or `consume_remote`.
    pub fn watermark(&self) -> u64 {
        self.shared.head.load(Ordering::Acquire)
    }

    /// Push a new value onto the queue.
    ///
    /// # Safety
    /// This is a local method. It may not be called concurrently with any other
    /// `*_local` methods.
    pub unsafe fn push_local(&self, value: T) {
        let local = unsafe { &mut *self.local.get() };
        local.push(&self.shared, value);
    }

    /// Consume values from the queue.
    ///
    /// Returns an iterator over the consumed values, if any.
    ///
    /// # Safety
    /// This is a local method. It may not be called concurrently with any other
    /// `*_local` method calls.
    ///
    /// `watermark` must have been returned by a previous call to
    /// [`watermark()`](LcalQueue::watermark) for this `LocalQueue` instance.
    ///
    /// Note that it is safe to call `consume_local` while the returned
    /// [`Drain`] iterator is still alive.
    pub unsafe fn consume_local(&self, limit: u64, watermark: u64) -> Option<Drain<T>> {
        let local = unsafe { &mut *self.local.get() };
        local.consume_local(&self.shared, limit, watermark)
    }

    /// Consume values from the queue remotely.
    ///
    /// Returns an iterator over the consumed values, if any.
    ///
    /// # Safety
    /// This is a remote method. It may not be called concurrently with any
    /// other `consume_remote` method calls. Calling it concurrently with
    /// `*_local` methods is OK.
    ///
    /// `watermark` must have been returned by a previous call to
    /// [`watermark()`](LcalQueue::watermark) for this `LocalQueue` instance.
    ///
    /// Note that it is safe to call `consume_remote` while the returned
    /// [`Drain`] iterator is still alive.
    pub unsafe fn consume_remote(&self, limit: u64, watermark: u64) -> Option<Drain<T>> {
        self.shared.consume_remote(limit, watermark)
    }
}

unsafe impl<T: Send> Send for LocalQueue<T> {}
unsafe impl<T: Sync> Sync for LocalQueue<T> {}

struct Chunk<T> {
    data: Vec<UnsafeCell<MaybeUninit<T>>>,
}

impl<T> Chunk<T> {
    pub fn new(len: usize) -> Self {
        let mut data = Vec::with_capacity(len);

        // SAFETY: We just set the capacity of data above and its element type
        //         is MaybeUninit so this is safe.
        unsafe { data.set_len(len) };

        Self { data }
    }
}

struct Shared<T> {
    chunk: ArcSwap<Chunk<T>>,

    /// Current head index for the ringbuffer.
    ///
    /// When we insert a new value into the ringbuffer we increment this index.
    head: AtomicU64,

    /// Tail index for the ring buffer.
    ///
    /// This is the last index for the values that are still in the ringbuffer.
    tail: AtomicU64,

    /// Remote watermark.
    ///
    /// All values below this index will no longer be ever accessed by the
    /// remote thread.
    remote: AtomicU64,
}

impl<T> Shared<T> {
    fn acquire_range(&self, remote: bool, limit: u64, watermark: u64) -> Option<Range<u64>> {
        if limit == 0 {
            return None;
        }

        let head = self.head.load(Ordering::Acquire);
        let mut tail = self.tail.load(Ordering::Acquire);

        if remote {
            // Preemptively load the current tail into remote before doing anything.
            self.remote.store(tail, Ordering::Release);
        }

        let watermark = head.min(watermark);
        let mut target;
        let start = loop {
            if watermark <= tail {
                if remote {
                    // We didn't load anything, mark us as inactive in remote
                    self.remote.store(u64::MAX, Ordering::Release);
                }
                return None;
            }

            target = tail.saturating_add(limit).min(watermark);
            match self
                .tail
                .compare_exchange_weak(tail, target, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(prev) => break prev,
                Err(prev) => tail = prev,
            }
        };

        if remote {
            // Update the remote header to the actual start of our range.
            self.remote.store(start, Ordering::Release);
        }

        // println!(
        //     "acquired {: <6} range {:03}..{:03} (queue {:?})",
        //     if remote { "remote" } else { "local" },
        //     start,
        //     target,
        //     self as *const _
        // );

        Some(start..target)
    }

    /// Acquire a range of indices so that we can safely remove them from the
    /// queue.
    ///
    /// This variant is meant for local queue access and so it does not update
    /// the remote index.
    fn acquire_local_range(&self, limit: u64, watermark: u64) -> Option<Range<u64>> {
        self.acquire_range(false, limit, watermark)
    }

    /// Acquire a range of indices so that we can safely remove them from the
    /// queue.
    fn acquire_remote_range(&self, limit: u64, watermark: u64) -> Option<Range<u64>> {
        self.acquire_range(true, limit, watermark)
    }

    pub fn consume_remote(&self, limit: u64, watermark: u64) -> Option<Drain<T>> {
        let range = self.acquire_remote_range(limit, watermark)?;
        let chunk = self.chunk.load();
        let indices = range_indices(range, chunk.data.len());

        Some(Drain {
            indices,
            chunk,
            remote: true,
            shared: self,
        })
    }
}

struct Local<T> {
    chunk: Arc<Chunk<T>>,
}

impl<T> Local<T> {
    pub fn push(&mut self, shared: &Shared<T>, value: T) {
        let len = self.chunk.data.len();

        let head = shared.head.load(Ordering::Relaxed);
        let tail = shared.tail.load(Ordering::Acquire);
        let remote = shared.remote.load(Ordering::Acquire);

        if tail.min(remote) + len as u64 == head {
            self.expand(shared);
        }

        // Make sure to use the new length if we expanded.
        let len = self.chunk.data.len();
        let chunk = &self.chunk;

        let index = (head % len as u64) as usize;

        // SAFETY: This slot is outside of the valid range
        unsafe { (*chunk.data[index].get()).write(value) };

        shared.head.store(head + 1, Ordering::Release);
    }

    pub fn consume_local<'a>(
        &mut self,
        shared: &'a Shared<T>,
        limit: u64,
        watermark: u64,
    ) -> Option<Drain<'a, T>> {
        let range = shared.acquire_local_range(limit, watermark)?;
        let chunk = self.chunk.clone();
        let indices = range_indices(range, chunk.data.len());

        Some(Drain {
            indices,
            chunk: Guard::from_inner(chunk),
            remote: false,
            shared,
        })
    }

    fn expand(&mut self, shared: &Shared<T>) {
        let len = self.chunk.data.len();
        let new_len = len * 2;

        let mut chunk = Chunk::new(new_len);
        self.copy_data(shared, &mut chunk.data);

        let chunk = Arc::new(chunk);
        self.chunk = chunk.clone();
        shared.chunk.store(chunk);
    }

    fn copy_data(&mut self, shared: &Shared<T>, dest: &mut [UnsafeCell<MaybeUninit<T>>]) {
        let source = &self.chunk.data;

        let src_len = source.len();
        let dst_len = dest.len();

        assert_eq!(dst_len % src_len, 0);

        // No synchronization needed here since we are the only ones to write
        // to the shared buffer.
        let head = shared.head.load(Ordering::Relaxed);
        let tail = shared.tail.load(Ordering::Relaxed);

        if head == tail {
            return;
        }

        let head_src = (head % src_len as u64) as usize;
        let tail_src = (tail % src_len as u64) as usize;

        let head_dst = (head % dst_len as u64) as usize;
        let tail_dst = (tail % dst_len as u64) as usize;

        // Source segment is contiguous so we can copy it in one shot.
        if tail_src < head_src {
            unsafe {
                let src = source.as_ptr().add(tail_src);
                let dst = dest.as_mut_ptr().add(tail_dst);

                std::ptr::copy_nonoverlapping(src, dst, head_src - tail_src);
            }
        } else {
            unsafe {
                let src = source.as_ptr().add(tail_src);
                let dst = dest.as_mut_ptr().add(tail_dst);

                std::ptr::copy_nonoverlapping(src, dst, src_len - tail_src);
            }

            // Not contiguous in source buffer but contiguous in dest buffer
            if tail_dst < head_dst {
                unsafe {
                    let src = source.as_ptr();
                    let dst = dest.as_mut_ptr().add(src_len);

                    std::ptr::copy_nonoverlapping(src, dst, head_src);
                }
            } else {
                unsafe {
                    let src = source.as_ptr();
                    let dst = dest.as_mut_ptr();

                    std::ptr::copy_nonoverlapping(src, dst, head_src);
                }
            }
        }
    }
}

fn range_indices(range: Range<u64>, len: usize) -> Chain<Range<usize>, Range<usize>> {
    let len = len as u64;

    if range.is_empty() {
        return (0..0).chain(0..0);
    }

    let start_idx = (range.start % len) as usize;
    let end_idx = (range.end % len) as usize;

    if start_idx < end_idx {
        (start_idx..end_idx).chain(0..0)
    } else {
        (start_idx..len as usize).chain(0..end_idx)
    }
}

pub struct Drain<'a, T> {
    indices: Chain<Range<usize>, Range<usize>>,
    chunk: Guard<Arc<Chunk<T>>>,
    remote: bool,
    shared: &'a Shared<T>,
}

impl<T> Iterator for Drain<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.indices.next()?;
        let slot = &self.chunk.data[index];

        // SAFETY: This is valid provided that we were solely handed a valid range of
        //         indices.
        Some(unsafe { (*slot.get()).assume_init_read() })
    }
}

impl<T> Drop for Drain<'_, T> {
    fn drop(&mut self) {
        while let Some(_) = self.next() {}

        if self.remote {
            self.shared.remote.store(u64::MAX, Ordering::Release);
        }
    }
}
