use std::mem::MaybeUninit;
use std::ops::Range;
use std::slice::SliceIndex;

use crate::shim::UnsafeCell;

type ChunkItem<T> = UnsafeCell<MaybeUninit<T>>;

pub(crate) struct Chunk<T> {
    data: Vec<ChunkItem<T>>,
}

impl<T> Chunk<T> {
    pub fn new(len: usize) -> Self {
        let mut data = Vec::with_capacity(len);
        data.resize_with(len, || UnsafeCell::new(MaybeUninit::uninit()));

        Self { data }
    }

    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    pub fn get<I>(&self, index: I) -> &I::Output
    where
        I: SliceIndex<[ChunkItem<T>]>,
    {
        &self.data[index]
    }

    pub fn get_mut<I>(&mut self, index: I) -> &mut I::Output
    where
        I: SliceIndex<[ChunkItem<T>]>,
    {
        &mut self.data[index]
    }

    pub fn get_logical(&self, index: u64) -> &ChunkItem<T> {
        let len = self.data.len() as u64;
        &self.data[(index % len) as usize]
    }

    /// Translate a logical range into one or two physical ranges.
    ///
    /// The ranges returned are guaranteed to not overlap.
    pub fn translate_range(&self, range: Range<u64>) -> (Range<usize>, Option<Range<usize>>) {
        let len = self.data.len() as u64;

        assert!(range.end - range.start <= len, "({range:?}).len() > {len}");

        let lo_idx: usize = (range.start % len) as usize;
        let hi_idx: usize = (range.end % len) as usize;

        if range.is_empty() {
            return (lo_idx..hi_idx, None);
        }

        match (lo_idx, hi_idx) {
            // The range reaches right to the end of the buffer but does not wrap around.
            (_, 0) => (lo_idx..self.data.len(), None),
            // We have one contiguous segment
            (lo, hi) if lo < hi => (lo..hi, None),
            // The range wraps around the end of the buffer.
            (lo, hi) => (hi..self.data.len(), Some(0..lo)),
        }
    }

    /// Drop all items stored in the buffer in the region specified by `range`.
    ///
    /// # Safety
    /// - `drop_range` may not be called concurrently with any other method that
    ///   modifies the elements in the provided range.
    /// - The items in the range must have live values contained within.
    pub unsafe fn drop_range(&self, range: Range<usize>) {
        for item in &self.data[range] {
            // SAFETY:
            // The safety contract on this function guarantess that
            // * Nobody else is modifying the item at the same time we are, and
            // * The item is initialized.
            //
            // This satifies the requirements for both UnsafeCell::get and
            // MaybeUninit::assume_init_drop.
            item.with_mut(|item| unsafe { (*item).assume_init_drop() });
        }
    }

    /// Copy data within the provided range from the source chunk to this one.
    ///
    /// # Safety
    /// - The items within the region specified by `range` must not be modified
    ///   concurrently.
    ///
    /// # Panics
    /// - Panics if `range` is larger than either `src.capacity()` or
    ///   `dst.capacity()`.
    pub unsafe fn copy_from(&mut self, src: &Self, range: Range<u64>) {
        let dst = self;

        let len = range.end - range.start;

        assert!(len <= src.capacity() as u64, "{len} > {}", src.capacity());
        assert!(len <= dst.capacity() as u64, "{len} > {}", dst.capacity());

        let (src1, src2) = src.translate_range(range.clone());
        let (dst1, dst2) = dst.translate_range(range.clone());

        match (src2, dst2) {
            // Easy case: both segments are contiguous
            (None, None) => move_uninit_slice(dst.get_mut(dst1), src.get(src1)),
            // dst contiguous, src is not
            (Some(src2), None) => {
                let (d1, d2) = dst.get_mut(dst1).split_at_mut(src1.len());

                move_uninit_slice(d1, src.get(src1));
                move_uninit_slice(d2, src.get(src2));
            }
            // src contiguous, dst is not
            (None, Some(dst2)) => {
                let (s1, s2) = src.get(src1).split_at(dst1.len());

                move_uninit_slice(dst.get_mut(dst1), s1);
                move_uninit_slice(dst.get_mut(dst2), s2);
            }
            // neither is contiguous
            (Some(src2), Some(dst2)) if src1.len() < dst1.len() => {
                let (d1, d2) = dst.get_mut(dst1).split_at_mut(src1.len());
                let (s1, s2) = src.get(src2).split_at(d2.len());

                move_uninit_slice(d1, src.get(src1));
                move_uninit_slice(d2, s1);
                move_uninit_slice(dst.get_mut(dst2), s2);
            }
            (Some(src2), Some(dst2)) if src1.len() > dst1.len() => {
                let (s1, s2) = src.get(src1).split_at(dst1.len());
                move_uninit_slice(dst.get_mut(dst1), s1);

                let (d1, d2) = dst.get_mut(dst2).split_at_mut(s2.len());
                move_uninit_slice(d1, s2);
                move_uninit_slice(d2, src.get(src2));
            }
            (Some(src2), Some(dst2)) /* if src1.len() == dst1.len() */ => {
                move_uninit_slice(dst.get_mut(dst1), src.get(src1));
                move_uninit_slice(dst.get_mut(dst2), src.get(src2));
            }
        }
    }
}

fn move_uninit_slice<T>(dst: &mut [ChunkItem<T>], src: &[ChunkItem<T>]) {
    assert_eq!(dst.len(), src.len());

    cfg_if! {
        if #[cfg(loom)] {
            // Manually implement the memcpy here so that loom understands it.
            for (dst, src) in dst.iter_mut().zip(src.iter()) {
                unsafe { *dst.get_mut().deref() = src.with(|p| std::ptr::read(p)) };
            }
        } else {
            // SAFETY: Since we already have valid slices and dst is &mut we meet all the
            //         preconditions of copy_nonoverlapping so this is safe.
            unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), dst.len()) }

        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_range_on_border() {
        let chunk = Chunk::<()>::new(4);
        let (rng1, rng2) = chunk.translate_range(2..4);

        assert_eq!((rng1, rng2), (2..4, None));
    }
}
