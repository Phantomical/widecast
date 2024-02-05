use std::error::Error;
use std::fmt;
use std::sync::atomic::Ordering;

use crate::detail::{Drain, Reader, Shared, Writer};
use crate::shim::{Arc, AtomicBool};

/// Create a new channel with the requested initial capacity.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let channel = Arc::new(Channel {
        shared: Shared::new(capacity),
        closed: AtomicBool::new(false),
    });

    (
        Sender {
            writer: Writer::new(&channel.shared),
            channel: channel.clone(),
        },
        Receiver {
            reader: Reader::new(&channel.shared),
            channel,
        },
    )
}

struct Channel<T> {
    shared: Shared<T>,
    closed: AtomicBool,
}

impl<T> Channel<T> {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release)
    }
}

pub struct Sender<T> {
    channel: Arc<Channel<T>>,
    writer: Writer<T>,
}

impl<T> Sender<T> {
    /// Checks if the channel has been closed.
    ///
    /// This happens when the [`Receiver`] is dropped, or when the
    /// [`Receiver::close`] method is called.
    pub fn is_closed(&self) -> bool {
        self.channel.is_closed()
    }

    /// Send a value into the channel.
    ///
    /// If the channel does not currently have enough capacity to accomodate the
    /// message then it will be resized in order to store it.
    ///
    /// Returns the queue index that the value was inserted at. This can be
    /// later used in a call to [`drain_to`]
    ///
    /// # Errors
    /// If the receive half of the channel is closed, either because [`close`]
    /// was called or the [`Receiver`] handle was dropped, then this
    /// function will return an error.
    ///
    /// [`drain_to`]: Receiver::drain_to
    /// [`close`]: Receiver::close
    pub fn send(&mut self, value: T) -> Result<u64, SendError<T>> {
        if self.is_closed() {
            return Err(SendError(value));
        }

        Ok(self.writer.push(&self.channel.shared, value))
    }

    /// Close the channel.
    ///
    /// Any future calls to [`send`] will result in an error. The [`Receiver`]
    /// handle will be able to remove any values currently stored in the channel
    /// but after that [`recv`], [`drain`], and [`drain_to`] will all exit with
    /// either [`RecvError::Disconnected`] or [`DrainError`].
    ///
    /// [`send`]: Sender::send
    /// [`recv`]: Receiver::recv
    /// [`drain`]: Receiver::drain
    /// [`drain_to`]: Receiver::drain_to
    pub fn close(&mut self) {
        self.channel.close();
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.channel.close();
    }
}

pub struct Receiver<T> {
    channel: Arc<Channel<T>>,
    reader: Reader<T>,
}

impl<T> Receiver<T> {
    /// Checks if the channel has been closed.
    ///
    /// This can happen either when [`close`] is called or when the [`Sender`]
    /// is dropped.
    ///
    /// [`close`]: Receiver::close
    pub fn is_closed(&self) -> bool {
        self.channel.is_closed()
    }

    /// Get the current head offset of the channel.
    ///
    /// This is the index at which the next value will be inserted into the
    /// channel. You can use
    pub fn head(&self) -> u64 {
        self.channel.shared.head()
    }

    /// Get the current tail offset of the channel.
    ///
    /// This is the index of the last value currently stored in the channel, if
    /// there is one. If the channel is empty then this will be equal to
    /// [`head()`]
    ///
    /// [`head()`]: Receiver::head
    pub fn tail(&self) -> u64 {
        self.reader.tail()
    }

    /// Receives the next value from the channel.
    ///
    /// This method will never block.
    ///
    /// If the channel is closed but there are still messages in the channel
    /// buffer then this method will return the messages before it returns
    /// [`RecvError::Disconnected`].
    ///
    /// # Errors
    /// - If the channel is empty then [`RecvError::Empty`] is returned.
    /// - If the channel is both empty and closed then
    ///   [`RecvError::Disconnected`] is returned.
    pub fn recv(&mut self) -> Result<T, RecvError> {
        match self.reader.pop(&self.channel.shared) {
            Some(value) => Ok(value),
            None if self.is_closed() => Err(RecvError::Disconnected),
            None => Err(RecvError::Empty),
        }
    }

    /// Drain all items from the channel.
    ///
    /// # Errors
    /// - If the channel is both empty and closed then [`DrainError`] will be
    ///   returned.
    pub fn drain(&mut self) -> Result<Drain<T>, DrainError> {
        self.drain_to(u64::MAX)
    }

    /// Drain the channel up to the `watermark`.
    ///
    /// This method will never block.
    ///
    /// The watermark denotes a global value index of all values sent via the
    /// channel. Useful watermark values to use may include:
    /// - a previous value of [`head`], or,
    /// - an index returned by [`Sender::send`].
    ///
    /// Any `u64` value may be used and it will automatically be clamped into
    /// the range of valid indices.
    ///
    /// # Errors
    /// - If the channel is both empty and closed then [`DrainError`] will be
    ///   returned.
    ///
    /// [`head`]: Receiver::head
    pub fn drain_to(&mut self, watermark: u64) -> Result<Drain<T>, DrainError> {
        let drain = self.reader.consume(&self.channel.shared, watermark);

        if drain.len() == 0 && self.channel.is_closed() && self.channel.shared.is_empty() {
            return Err(DrainError);
        }

        Ok(drain)
    }

    /// Close the channel.
    ///
    /// Future calls to [`Sender::send`] will result in an error. You can still
    /// remove any existing values contained within the channel via [`recv`],
    /// [`drain`], and [`drain_to`] but once the channel is empty those will
    /// return an error as well.
    ///
    /// [`recv`]: Receiver::recv
    /// [`drain`]: Receiver::drain
    /// [`drain_to`]: Receiver::drain_to
    pub fn close(&mut self) {
        self.channel.close();
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.channel.close();
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub struct SendError<T>(pub T);

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendError").finish_non_exhaustive()
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("channel closed")
    }
}

impl<T> Error for SendError<T> {}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecvError {
    Empty,
    Disconnected,
}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "receiving on an empty channel",
            Self::Disconnected => "receiving on a closed channel",
        })
    }
}

impl Error for RecvError {}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DrainError;

impl fmt::Display for DrainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("receiving on a closed channel")
    }
}

impl Error for DrainError {}
