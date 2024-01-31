//! Widecast is a single-writer multiple-reader broadcast channel that is
//! optimized for low tail latencies with large numbers (100 - 10k+) of
//! subscribers.
//!
//! # Overview
//! A [`Sender`] is used to broadcast values to all connected [`Receiver`]s.
//! When a value is sent all [`Receiver`] handles are notified and will receive
//! the value. Values are stored in an internal ringbuffer until they are
//! overwritten.
//!
//! A channel is created by calling [`Sender::new`] and specifying the maximum
//! number of messages that the channel can hold. You can create [`Receiver`]
//! handles by calling [`Sender::subscribe`] or cloning an existing handle.
//! [`Receiver`]s created via [`Sender::subscribe`] will receive any values sent
//! **after** the call to subscribe, cloned receivers have the same position as
//! the one they were cloned from.
//!
//! # Lagging
//! The channel only retains a limited number of messages. It is possible that a
//! [`Receiver`] falls too far behind and fails to read a message before it is
//! overwritten. In that case, the receiver will return [`RecvError::Lagged`]
//! the next time [`recv`] is called. The receiver's position will then be
//! updated to the oldest value contained within the channel.
//!
//! # Closing
//! When the [`Sender`] is dropped no new can be sent and [`recv`] calls will
//! return [`RecvError::Closed`] once the receiver has consumed all messages
//! waiting in the channel.
//!
//! [`recv`]: Receiver::recv
//!
//! # Example
//! Basic usage:
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! use widecast::Sender;
//!
//! let mut tx = Sender::new(16);
//! let mut rx1 = tx.subscribe();
//! let mut rx2 = tx.subscribe();
//!
//! tokio::spawn(async move {
//!     assert_eq!(rx1.recv().await.unwrap(), 10);
//!     assert_eq!(rx1.recv().await.unwrap(), 20);
//! });
//!
//! tokio::spawn(async move {
//!     assert_eq!(rx2.recv().await.unwrap(), 10);
//!     assert_eq!(rx2.recv().await.unwrap(), 20);
//! });
//!
//! tx.send(10);
//! tx.send(20);
//! # }
//! ```
//!
//! # Differences from tokio's broadcast channel
//! While widecast can be used as a drop in replacement for some use cases of
//! the tokio broadcast channel, it is not the same.
//!
//! - [`Sender`] is not clonable and [`Sender::send`] requires mutable access.
//!   This means that it cannot be easily shared between multiple threads. If
//!   you wish to use sender from multiple threads or async tasks consider using
//!   `Arc<Mutex<Sender<T>>>` instead.
//! - Tokio's broadcast channel eagerly drops messages stored within the channel
//!   once all receivers have read them. Widecast does not do this. Instead,
//!   messages are only dropped when their slot in the underlying ringbuffer is
//!   overwritten.
//! - Tokio's broadcast channel returns a clone of the original message value.
//!   Widecast returns a [`Guard`] that allows you to access the original value
//!   stored in the channel. As such, it does not require that `T` be `Clone`.
//!
//! # Implementation Details
//! Internally, the broadcast channel is built using two different parts:
//! - Values are stored using a ringbuffer of `Arc`s. This allows receivers to
//!   keep a copy of the `Arc` without having to worry about the value being
//!   moved around. Writers atomically replace the `Arc` within the ringbuffer.
//! - Notifications are performed using the [`masswake`] crate. See its
//!   documentation for further implementation details.
//!
//! Note that for larger numbers of receivers the vast majority of the time is
//! spent waking up tasks via the [`masswake`] crate.

#![warn(missing_docs)]

use std::sync::atomic::{AtomicBool, Ordering};

use arcbuf::{ArcCache, IndexError};
use masswake::MassNotify;
use receiver::RawGuard;

use crate::arcbuf::ArcBuffer;

/// Helper macro used to silence `unused_import` warnings when an item is
/// only imported in order to refer to it within a doc comment.
macro_rules! used_in_docs {
    ($( $item:ident ),*) => {
        const _: () = {
            #[allow(unused_imports)]
            mod dummy {
                $( use super::$item; )*
            }
        };
    };
}

mod arcbuf;
mod error;
mod receiver;
mod refcnt;
mod sender;

pub use crate::error::{RecvError, TryRecvError};
pub use crate::receiver::{Guard, OwnedGuard, Receiver};
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

    pub(crate) fn get(&self, watermark: u64) -> Result<RawGuard<T>, GetError> {
        match self.buffer.get(watermark) {
            Ok(guard) => Ok(guard),
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
