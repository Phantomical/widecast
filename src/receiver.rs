use std::ops::Deref;
use std::task::{Context, Poll};
use std::{fmt, mem};

use masswake::Interest;
use triomphe::Arc;

use crate::arcbuf::IndexError;
use crate::refcnt::ArcWrap;
use crate::{GetError, RecvError, Shared, TryRecvError};

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

    pub async fn recv(&mut self) -> Result<Guard<T>, RecvError> {
        std::future::poll_fn(|cx| self.poll_recv(cx)).await
    }

    pub fn try_recv(&mut self) -> Result<Guard<T>, TryRecvError> {
        match self.try_recv_raw() {
            Ok(guard) => Ok(guard),
            Err(GetError::Closed) => Err(TryRecvError::Closed),
            Err(GetError::Index(IndexError::Outdated(skipped))) => {
                Err(TryRecvError::Lagged(skipped))
            }
            Err(GetError::Index(IndexError::Invalid(_))) => Err(TryRecvError::Empty),
        }
    }

    pub fn resubscribe(&self) -> Self {
        Self::new(self.shared.clone())
    }
}

impl<T> Receiver<T> {
    fn try_recv_raw(&mut self) -> Result<Guard<T>, GetError> {
        match self.shared.get(self.watermark) {
            Ok(guard) => {
                debug_assert!(guard.guard.0.is_some());
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

    fn poll_once(&mut self) -> Poll<Result<Guard<T>, RecvError>> {
        Poll::Ready(match self.try_recv_raw() {
            Ok(guard) => Ok(guard),
            Err(GetError::Closed) => Err(RecvError::Closed),
            Err(GetError::Index(IndexError::Outdated(skipped))) => Err(RecvError::Lagged(skipped)),
            Err(GetError::Index(IndexError::Invalid(_))) => return Poll::Pending,
        })
    }

    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<Guard<T>, RecvError>> {
        if let Poll::Ready(ready) = self.poll_once() {
            // self.interest.assist();
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

pub struct Guard<T> {
    guard: arc_swap::Guard<ArcWrap<T>, arc_swap::DefaultStrategy>,
}

impl<T> Guard<T> {
    pub(crate) fn new(guard: arc_swap::Guard<ArcWrap<T>, arc_swap::DefaultStrategy>) -> Self {
        Self { guard }
    }
}

impl<T> Deref for Guard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match &self.guard.0 {
            Some(value) => value,
            None => unreachable!(),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Guard<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}
