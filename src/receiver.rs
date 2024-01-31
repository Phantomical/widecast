use std::marker::PhantomData;
use std::ops::Deref;
use std::task::{Context, Poll};
use std::{fmt, mem};

use masswake::Interest;
use triomphe::Arc;

use crate::arcbuf::IndexError;
use crate::refcnt::ArcWrap;
use crate::{GetError, RecvError, Sender, Shared, TryRecvError};

used_in_docs!(Sender);

/// The receiving half of a broadcast queue.
///
/// Create one by calling [`Sender::subscribe`].
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    interest: Interest,
    watermark: u64,
}

impl<T> Receiver<T> {
    pub(crate) fn new(shared: Arc<Shared<T>>) -> Self {
        Self {
            watermark: shared.buffer.head(),
            interest: Interest::new(&shared.notify),
            shared,
        }
    }

    /// Receive the next value for this receiver.
    ///
    /// This returns a [`Guard`] - a temporary handle to the stored value.
    ///
    /// ## Errors
    /// - If the channel is closed then this will return [`RecvError::Closed`].
    /// - If the receiver handle has fallen behind the oldest value stored in
    ///   the channel then [`RecvError::Lagged`] will be returned. The internal
    ///   cursor will then be updated to point to the oldest value still held
    ///   within the channel. A subsequent call to `recv` will return this value
    ///   unless it has been overwritten.
    ///
    /// ## Cancel Safety
    /// This method is cancel safe. If the returned future is dropped without
    /// having been polled to completion it is guaranteed that no messages were
    /// received on this channel.
    pub async fn recv(&mut self) -> Result<Guard<T>, RecvError> {
        std::future::poll_fn(|cx| self.poll_recv(cx))
            .await
            .map(Guard::new)
    }

    /// Attempt to return a pending value on this receiver without awaiting.
    ///
    /// This returns a [`Guard`] - a temporary handle to the stored value.
    ///
    /// ## Errors
    /// This has three different failure cases as compared to the two error
    /// cases of [`recv`]:
    /// - If there are no new values within the channel then this will return
    ///   [`TryRecvError::Empty`].
    /// - If the channel is closed then this will return
    ///   [`TryRecvError::Closed`].
    /// - If the receiver handle has fallen behind the oldest value stored in
    ///   the channel then [`TryRecvError::Lagged`] will be returned. The
    ///   internal cursor will then be updated to point to the oldest value
    ///   still held within the channel. A subsequent call to `recv` will return
    ///   this value unless it has been overwritten.
    ///
    /// [`recv`]: Receiver::recv
    pub fn try_recv(&mut self) -> Result<Guard<T>, TryRecvError> {
        match self.try_recv_raw() {
            Ok(guard) => Ok(Guard::new(guard)),
            Err(GetError::Closed) => Err(TryRecvError::Closed),
            Err(GetError::Index(IndexError::Outdated(skipped))) => {
                Err(TryRecvError::Lagged(skipped))
            }
            Err(GetError::Index(IndexError::Invalid(_))) => Err(TryRecvError::Empty),
        }
    }

    /// Re-subscribe to the channel starting from the current tail element.
    ///
    /// The returned receiver handle will see all values sent **after** it was
    /// resubscribed. This will not include elements that are in the queue of
    /// the current receiver.
    ///
    /// If you instead wish to create a new receiver at the same position as the
    /// current one you can clone the receiver instead.
    pub fn resubscribe(&self) -> Self {
        Self::new(self.shared.clone())
    }
}

impl<T> Receiver<T> {
    fn try_recv_raw(&mut self) -> Result<RawGuard<T>, GetError> {
        match self.shared.get(self.watermark) {
            Ok(guard) => {
                debug_assert!(guard.0.is_some());
                self.watermark += 1;
                Ok(guard)
            }
            Err(GetError::Index(IndexError::Outdated(tail))) => {
                let skipped = tail - mem::replace(&mut self.watermark, tail);
                Err(GetError::Index(IndexError::Outdated(skipped)))
            }
            Err(e) => Err(e),
        }
    }

    fn poll_once(&mut self) -> Poll<Result<RawGuard<T>, RecvError>> {
        Poll::Ready(match self.try_recv_raw() {
            Ok(guard) => Ok(guard),
            Err(GetError::Closed) => Err(RecvError::Closed),
            Err(GetError::Index(IndexError::Outdated(skipped))) => Err(RecvError::Lagged(skipped)),
            Err(GetError::Index(IndexError::Invalid(_))) => return Poll::Pending,
        })
    }

    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<RawGuard<T>, RecvError>> {
        if let Poll::Ready(ready) = self.poll_once() {
            return Poll::Ready(ready);
        }

        self.interest.register(cx);
        self.poll_once()
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            interest: Interest::new(&self.shared.notify),
            watermark: self.watermark,
        }
    }
}

pub(crate) type RawGuard<T> = arc_swap::Guard<ArcWrap<T>, arc_swap::DefaultStrategy>;

/// An owned version of [`Guard`].
///
/// This is just a type alias of [`triomphe::Arc`] so that users of widecast can
/// refer to it without having to depend on triomphe directly.
pub type OwnedGuard<T> = Arc<T>;

/// A reference to a value stored within a broadcast queue.
///
/// If you need to store the guard for a longer period of time consider
/// converting it into its internal [`OwnedGuard`] by calling [`into_inner`].
///
/// [`into_inner`]: Guard::into_inner
// Note: We don't actually use the lifetime here. It will just make it more
// likely that users will convert the
pub struct Guard<'a, T> {
    guard: RawGuard<T>,
    _marker: PhantomData<&'a ()>,
}

impl<'a, T> Guard<'a, T> {
    pub(crate) fn new(guard: RawGuard<T>) -> Self {
        Self {
            guard,
            _marker: PhantomData,
        }
    }

    /// Convert this `Guard` into its inner `OwnedGuard`.
    ///
    /// This will at most involve adjusting the reference count of the internal
    /// `Arc`.
    pub fn into_inner(this: Self) -> OwnedGuard<T> {
        match arc_swap::Guard::into_inner(this.guard).0 {
            Some(value) => value,
            None => unreachable!(),
        }
    }
}

impl<T> From<Guard<'_, T>> for Arc<T> {
    fn from(value: Guard<'_, T>) -> Self {
        Guard::into_inner(value)
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match &self.guard.0 {
            Some(value) => value,
            None => unreachable!(),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Guard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}
