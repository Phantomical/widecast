//! A single-writer multiple-reader broadcast queue optimized for large numbers
//! of readers.

use std::sync::atomic::{AtomicBool, Ordering};

use arcbuf::{ArcCache, IndexError};
use masswake::MassNotify;

use crate::arcbuf::ArcBuffer;

mod arcbuf;
mod error;
mod receiver;
mod refcnt;
mod sender;

pub use crate::error::{RecvError, TryRecvError};
pub use crate::receiver::{Guard, Receiver};
pub use crate::sender::Sender;

struct Shared<T> {
    buffer: ArcBuffer<T>,
    closed: AtomicBool,
    notify: MassNotify,
}

impl<T> Shared<T> {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: ArcBuffer::with_capacity(capacity),
            closed: AtomicBool::new(false),
            notify: MassNotify::new(),
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
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
