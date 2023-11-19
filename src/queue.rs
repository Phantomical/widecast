use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Waker;

use arc_swap::{ArcSwapAny, Guard};
use crossbeam_utils::CachePadded;
use triomphe::{Arc, UniqueArc};

use crate::refcnt::ArcWrap;

const BLOCK_LEN: u64 = 1024;
const CLEANUP_INTERVAL: u32 = 1024;

type WakerGuard = Guard<ArcWrap<Block<Waker>>>;

pub struct WakerQueue {
    local: CachePadded<UnsafeCell<Local>>,
    shared: Shared,
}

impl WakerQueue {
    pub fn new() -> Self {
        Self {
            local: CachePadded::new(UnsafeCell::new(Local::new())),
            shared: Shared::new(),
        }
    }

    pub unsafe fn push(&self, waker: Waker) {
        self.local().push(&self.shared, waker);
    }

    pub fn set_watermark(&self) {
        self.shared.set_watermark();
    }

    pub unsafe fn wake_local(&self, limit: u64) {
        self.local().wake_local(&self.shared, limit)
    }

    pub fn wake_remote(&self, limit: u64) {
        self.shared.wake_remote(limit)
    }

    unsafe fn local(&self) -> &mut Local {
        &mut *self.local.get()
    }
}

unsafe impl Send for WakerQueue {}
unsafe impl Sync for WakerQueue {}

impl Default for WakerQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WakerQueue {
    fn drop(&mut self) {
        let tail = *self.shared.tail.get_mut();
        let head = *self.shared.head.get_mut();

        let _ = Drain::new(tail..head, self.shared.block.load());
    }
}

struct Block<T> {
    base: u64,
    next: ArcSwapAny<ArcWrap<Self>>,
    data: [UnsafeCell<MaybeUninit<T>>; BLOCK_LEN as usize],
}

impl<T> Block<T> {
    pub fn new(base: u64) -> UniqueArc<Self> {
        use std::ptr;

        let mut uninit = UniqueArc::<Self>::new_uninit();
        let ptr = (*uninit).as_mut_ptr();

        unsafe {
            ptr::write(ptr::addr_of_mut!((*ptr).base), base);
            ptr::write(ptr::addr_of_mut!((*ptr).next), ArcSwapAny::default());

            std::mem::transmute(uninit)
        }
    }

    fn slot_for_index(&self, index: u64) -> &UnsafeCell<MaybeUninit<T>> {
        let offset = index - self.base;
        assert!(offset < BLOCK_LEN);

        &self.data[offset as usize]
    }
}

struct Shared {
    block: ArcSwapAny<ArcWrap<Block<Waker>>>,

    head: AtomicU64,
    tail: CachePadded<AtomicU64>,
    watermark: CachePadded<AtomicU64>,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            block: ArcSwapAny::default(),
            head: AtomicU64::new(0),
            tail: Default::default(),
            watermark: Default::default(),
        }
    }

    pub fn set_watermark(&self) -> u64 {
        let head = self.head.load(Ordering::Acquire);
        self.watermark.store(head, Ordering::Release);
        head
    }

    /// Wake tasks.
    pub fn wake_remote(&self, limit: u64) {
        let (range, block) = match self.acquire_range(limit) {
            Ok(range) => range,
            Err(_) => return,
        };

        for waker in Drain::new(range, block) {
            waker.wake();
        }
    }

    /// Acquire a range so that we can remove it from the queue.
    ///
    /// Returns Ok with the range indices if successful, or Err with the current
    /// tail value otherwise.
    fn acquire_range(&self, limit: u64) -> Result<(Range<u64>, WakerGuard), u64> {
        let watermark = self.watermark.load(Ordering::Acquire);
        let mut tail = self.tail.load(Ordering::Acquire);

        if limit == 0 || tail >= watermark {
            return Err(tail);
        }

        let block = self.block.load();

        let start = loop {
            if tail >= watermark {
                return Err(tail);
            }

            let target = tail.saturating_add(limit).min(watermark);
            match self.tail.compare_exchange_weak(
                tail,
                target,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(prev) => break prev,
                Err(prev) => tail = prev,
            }
        };

        let end = start.saturating_add(limit).min(watermark);
        Ok((start..end, block))
    }
}

struct Local {
    tail: Option<Arc<Block<Waker>>>,
    next_cleanup: u32,
}

impl Local {
    pub fn new() -> Self {
        Self {
            tail: None,
            next_cleanup: CLEANUP_INTERVAL,
        }
    }

    /// Push a new waker into the queue.
    pub fn push(&mut self, shared: &Shared, value: Waker) {
        let head = shared.head.load(Ordering::Relaxed);
        let block = match self.block_for_index(head) {
            Some(block) => block,
            None => {
                self.push_block(shared, head);
                self.block_for_index(head)
                    .expect("no block for head index even after pushing a new block")
            }
        };

        let slot = block.slot_for_index(head);

        // SAFETY: Slot is outside of the valid bounds of the array so nothing
        //         else will be accessing it concurrently.
        unsafe { (*slot.get()).write(value) };

        shared.head.store(head + 1, Ordering::Release);
    }

    pub fn wake_local(&mut self, shared: &Shared, limit: u64) {
        let (range, block) = match shared.acquire_range(limit) {
            Ok(range) => range,
            Err(_) => {
                self.next_cleanup -= 1;
                if self.next_cleanup == 0 {
                    self.pop_blocks(shared);
                }
                return;
            }
        };

        for waker in Drain::new(range, block) {
            waker.wake();
        }

        self.pop_blocks(shared);
    }

    fn pop_blocks(&mut self, shared: &Shared) {
        let tail = shared.tail.load(Ordering::Acquire);
        let mut block = shared.block.load();

        while let Some(current) = &block.0 {
            if tail < current.base + BLOCK_LEN {
                break;
            }

            block = current.next.load();
            shared.block.store(block.clone());

            if block.0.is_none() {
                self.tail = None;
                break;
            }
        }

        self.next_cleanup = CLEANUP_INTERVAL;
    }

    fn push_block(&mut self, shared: &Shared, base: u64) {
        debug_assert_eq!(base % BLOCK_LEN, 0);

        let block = Block::new(base).shareable();

        if let Some(tail) = self.tail.as_mut() {
            tail.next.store(block.clone().into());
            *tail = block;
        } else {
            self.tail = Some(block.clone());
            shared.block.store(block.into());
        }
    }

    fn block_for_index(&self, index: u64) -> Option<Arc<Block<Waker>>> {
        let tail = self.tail.as_ref()?;
        let offset = index - tail.base;

        if offset >= BLOCK_LEN {
            None
        } else {
            Some(tail.clone())
        }
    }
}

struct Drain {
    range: Range<u64>,
    block: WakerGuard,
}

impl Drain {
    fn new(range: Range<u64>, block: WakerGuard) -> Self {
        Self { range, block }
    }

    fn block(&self) -> &Block<Waker> {
        self.block.0.as_deref().unwrap()
    }

    fn advance_to_block(&mut self, index: u64) -> &UnsafeCell<MaybeUninit<Waker>> {
        let base = self.block().base;
        let mut offset = index - base;

        while offset >= BLOCK_LEN {
            let next = self.block().next.load();
            self.block = next;
            offset -= BLOCK_LEN;
        }

        self.block().slot_for_index(index)
    }
}

impl Iterator for Drain {
    type Item = Waker;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.range.next()?;
        let slot = self.advance_to_block(index);

        // SAFETY: Provided the indices are correct then this should be valid
        //         and should not be being modified concurrently.
        Some(unsafe { (*slot.get()).assume_init_read() })
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        while let Some(_) = self.next() {}
    }
}
