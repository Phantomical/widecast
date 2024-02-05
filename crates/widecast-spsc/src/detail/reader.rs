use std::iter::{Chain, FusedIterator};
use std::marker::PhantomData;
use std::mem::{self, MaybeUninit};
use std::ops::Range;
use std::sync::atomic::Ordering;

use super::chunk::Chunk;
use super::Shared;
use crate::shim::{Arc, Guard, UnsafeCell};

type ChunkGuard<T> = Guard<Arc<Chunk<T>>>;

pub(crate) struct Reader<T> {
    tail: u64,
    _marker: PhantomData<T>,
}

impl<T> Reader<T> {
    pub fn new(shared: &Shared<T>) -> Self {
        Self {
            tail: shared.tail.load(Ordering::Acquire),
            _marker: PhantomData,
        }
    }

    /// Get the saved tail value stored in this reader.
    ///
    /// This will always be equivalent to the tail value stored in [`Shared`].
    ///
    /// This is likely to be slightly faster than reading it from [`Shared`]
    /// because it does not need to perform an atomic load.
    pub fn tail(&self) -> u64 {
        self.tail
    }

    /// Acquire a range of indices in preparation from removing them from the
    /// queue.
    ///
    /// This will update `self.tail` but not `shared.tail`.
    ///
    /// # Parameters
    /// - `shared` - The shared portion of the queue.
    /// - `watermark` - The maximum index that can be acquired
    fn acquire_range(&mut self, shared: &Shared<T>, watermark: u64) -> (Range<u64>, ChunkGuard<T>) {
        let head = shared.head.load(Ordering::Acquire);

        // Need to load chunk after we load head but before we update tail.
        let chunk = shared.chunk.load();

        let watermark = head.min(watermark);
        let target = self.tail.max(watermark);
        let start = std::mem::replace(&mut self.tail, target);

        if target != start {
            shared.update_tail(target);
        }

        (start..target, chunk)
    }

    pub fn pop(&mut self, shared: &Shared<T>) -> Option<T> {
        let (range, chunk) = self.acquire_range(shared, self.tail + 1);
        if range.start == range.end {
            return None;
        }

        Drain::new(chunk, range, shared, self).next()
    }

    /// Acquire a draining iterator for all elements currently in the queue, up
    /// to `watermark`.
    pub fn consume<'a>(&'a mut self, shared: &'a Shared<T>, watermark: u64) -> Drain<'a, T> {
        let (range, chunk) = self.acquire_range(shared, watermark);

        Drain::new(chunk, range, shared, self)
    }
}

type ChunkIter<'a, T> = std::slice::Iter<'a, UnsafeCell<MaybeUninit<T>>>;

pub struct Drain<'a, T> {
    // Note: These actually refer to _chunk. The 'a lifetime on them is a lie.
    iter: Chain<ChunkIter<'a, T>, ChunkIter<'a, T>>,

    shared: &'a Shared<T>,
    remote: &'a mut Reader<T>,
    _chunk: Guard<Arc<Chunk<T>>>,

    #[cfg(loom)]
    _track: Option<loom::cell::MutPtr<Reader<T>>>,
}

impl<'a, T> Drain<'a, T> {
    fn new(
        chunk: Guard<Arc<Chunk<T>>>,
        range: Range<u64>,
        shared: &'a Shared<T>,
        remote: &'a mut Reader<T>,
    ) -> Self {
        let (range1, range2) = chunk.translate_range(range.clone());

        let slice1 = chunk.get(range1);
        let slice2 = range2.map(|r| chunk.get(r)).unwrap_or_default();
        let iter: Chain<ChunkIter<T>, ChunkIter<T>> = slice1.iter().chain(slice2.iter());

        Self {
            // SAFETY:
            // This extends the lifetime on iter1 and iter2 to 'a.
            //
            // We ensure that chunk is kept around until the struct is dropped so this is safe, the
            // lifetime on these iterators is just a lie. This is not an issue, though, since we do
            // not let the iterators escape the struct.
            iter: unsafe { mem::transmute(iter) },

            shared,
            remote,
            _chunk: chunk,

            #[cfg(loom)]
            _track: None,
        }
    }

    #[cfg(loom)]
    pub(crate) fn with_loom_ptr(mut self, track: loom::cell::MutPtr<Reader<T>>) -> Self {
        self._track = Some(track);
        self
    }

    fn read_slot(slot: &UnsafeCell<MaybeUninit<T>>) -> T {
        // SAFETY: When constructed we guarantee that all the items returned by iter
        //         belong to this Drain alone.
        slot.with(|slot| unsafe { (*slot).assume_init_read() })
    }
}

impl<T> Iterator for Drain<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(Self::read_slot)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        if !mem::needs_drop::<T>() {
            self.iter.nth(n).map(Self::read_slot)
        } else {
            DefaultIterator(self).nth(n)
        }
    }

    fn last(mut self) -> Option<Self::Item> {
        if !mem::needs_drop::<T>() {
            mem::take(&mut self.iter).last().map(Self::read_slot)
        } else {
            DefaultIterator(self).last()
        }
    }
}

impl<T> DoubleEndedIterator for Drain<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter.next_back().map(Self::read_slot)
    }

    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        if !mem::needs_drop::<T>() {
            self.iter.nth_back(n).map(Self::read_slot)
        } else {
            DefaultIterator(self).nth_back(n)
        }
    }
}

// While Chain doesn't implement ExactSizeIterator by default we do know that
// the two slices we're iterating over are disjoint so we can implement it.
impl<T> ExactSizeIterator for Drain<'_, T> {}

impl<T> FusedIterator for Drain<'_, T> {}

impl<T> Drop for Drain<'_, T> {
    fn drop(&mut self) {
        if mem::needs_drop::<T>() {
            // Normally this could be done more efficiently with drop_in_place. However,
            // when the writer expands the backing buffer it may end up copying memory that
            // is currently being read by a draining iterator.
            //
            // If we were to be running a drop_in_place concurrently with that copy then
            // that would be a data race and therefore UB. To avoid that we just read all
            // the elements and drop them here.
            for _ in self.by_ref() {}
        }

        // Update the tail pointer so that the writer can now use the space.
        self.shared
            .remote
            .store(self.remote.tail, Ordering::Release);
    }
}

unsafe impl<T: Send> Send for Drain<'_, T> {}
unsafe impl<T: Send> Sync for Drain<'_, T> {}

/// An iterator adapter that forwards everything except the required methods to
/// their default impls on `Iterator`.
struct DefaultIterator<I>(I);

impl<I> Iterator for DefaultIterator<I>
where
    I: Iterator,
{
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<I> DoubleEndedIterator for DefaultIterator<I>
where
    I: DoubleEndedIterator,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back()
    }
}
