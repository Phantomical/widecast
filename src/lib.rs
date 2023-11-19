//! A single-writer multiple-reader broadcast queue optimized for large numbers
//! of readers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Waker;

use arcbuf::{ArcCache, IndexError};
use crossbeam_queue::SegQueue;
use thread_local::ThreadLocal;

use crate::arcbuf::ArcBuffer;

mod arcbuf;
mod error;
mod receiver;
mod refcnt;
mod sender;

pub use crate::error::{RecvError, TryRecvError};
pub use crate::receiver::{Guard, Receiver};
pub use crate::sender::Sender;

type WakerQueue = SegQueue<Waker>;

struct Shared<T> {
    buffer: ArcBuffer<T>,
    closed: AtomicBool,

    queues: ThreadLocal<WakerQueue>,
}

impl<T> Shared<T> {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: ArcBuffer::with_capacity(capacity),
            closed: AtomicBool::new(false),
            queues: ThreadLocal::new(),
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    pub(crate) fn thread_queue(&self) -> &WakerQueue {
        self.queues.get_or_default()
    }

    pub(crate) fn get(&self, watermark: u64) -> Result<Guard<T>, GetError> {
        match self.buffer.get(watermark) {
            Ok(guard) => Ok(Guard::new(guard)),
            Err(IndexError::Invalid(_)) if self.is_closed() => Err(GetError::Closed),
            Err(e) => Err(GetError::Index(e)),
        }
    }
}

enum GetError {
    Index(IndexError),
    Closed,
}

#[cfg(test)]
mod tests;
