//! A single-writer multiple-reader broadcast queue optimized for large numbers
//! of readers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Waker;

use arcbuf::{ArcCache, IndexError};
use crossbeam_queue::SegQueue;

use crate::arcbuf::ArcBuffer;
use crate::thread_id::thread_id;

mod arcbuf;
mod error;
mod receiver;
mod refcnt;
mod sender;
mod thread_id;

pub use crate::error::{RecvError, TryRecvError};
pub use crate::receiver::{Guard, Receiver};
pub use crate::sender::Sender;

type WakerQueue = SegQueue<Waker>;

struct Shared<T> {
    buffer: ArcBuffer<T>,
    closed: AtomicBool,

    queues: [WakerQueue; 8],
}

impl<T> Shared<T> {
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    pub(crate) fn thread_queue(&self) -> &WakerQueue {
        let thread_id = (thread_id() % (self.queues.len() as u64)) as usize;
        &self.queues[thread_id]
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
