use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use crossbeam_queue::SegQueue;

use crate::arcbuf::{ArcBuffer, IndexError};
use crate::thread_id::thread_id;

mod arcbuf;
mod error;
mod thread_id;

pub use crate::error::{RecvError, TryRecvError};

struct Shared<T> {
    buffer: ArcBuffer<T>,
    closed: AtomicBool,

    queues: [SegQueue<Waker>; 8],
}

impl<T> Shared<T> {
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    pub(crate) fn thread_queue(&self) -> &SegQueue<Waker> {
        let thread_id = (thread_id() % (self.queues.len() as u64)) as usize;
        &self.queues[thread_id]
    }
}

pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Sender<T> {
    pub fn new(capacity: usize) -> Self {
        let shared = Arc::new(Shared {
            buffer: ArcBuffer::with_capacity(capacity),
            closed: AtomicBool::new(false),
            queues: std::array::from_fn(|_| SegQueue::new()),
        });

        Self { shared }
    }

    pub fn subscribe(&self) -> Receiver<T> {
        Receiver::new(self.shared.clone())
    }

    pub fn send(&mut self, value: T) {
        self.shared.buffer.push(Arc::new(value));
        self.wake_receivers();
    }

    pub fn bulk_send<I: Iterator<Item = T>>(&mut self, iter: I) {
        self.shared.buffer.push_bulk(iter.map(Arc::new));
        self.wake_receivers();
    }

    pub fn receiver_count(&self) -> usize {
        Arc::strong_count(&self.shared) - 1
    }

    fn wake_receivers(&self) {
        for queue in &self.shared.queues {
            // Protect against pathological conditions where polls are rapidly
            // being added as we are attempting to
            let max_wake = queue.len() * 2;

            for _ in 0..max_wake {
                let waker = match queue.pop() {
                    Some(waker) => waker,
                    None => break,
                };

                waker.wake();
            }
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
    }
}

pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    watermark: u64,
}

impl<T> Receiver<T> {
    pub(crate) fn new(shared: Arc<Shared<T>>) -> Self {
        Self {
            watermark: shared.buffer.head(),
            shared,
        }
    }

    pub fn try_recv(&mut self) -> Result<Guard<T>, TryRecvError> {
        match self.shared.buffer.get(self.watermark) {
            Ok(guard) => {
                self.watermark += 1;
                Ok(Guard { guard })
            }
            Err(IndexError::Invalid) if self.shared.is_closed() => Err(TryRecvError::Closed),
            Err(IndexError::Invalid) => Err(TryRecvError::Empty),
            Err(IndexError::Outdated(new_watermark)) => {
                let skipped = new_watermark - self.watermark;
                self.watermark = new_watermark;
                Err(TryRecvError::Lagged(skipped))
            }
        }
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            watermark: self.watermark,
        }
    }
}

pub struct Guard<T> {
    guard: arc_swap::Guard<Option<Arc<T>>, arc_swap::DefaultStrategy>,
}

impl<T> Deref for Guard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match &*self.guard {
            Some(value) => &value,
            None => unreachable!(),
        }
    }
}

struct Recv<'a, T> {
    rx: &'a mut Receiver<T>,
}

impl<'a, T> Future for Recv<'a, T> {
    type Output = Result<Guard<T>, RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.try_recv() {
            Ok(guard) => return Poll::Ready(Ok(guard)),
            Err(TryRecvError::Closed) => return Poll::Ready(Err(RecvError::Closed)),
            Err(TryRecvError::Lagged(skipped)) => {
                return Poll::Ready(Err(RecvError::Lagged(skipped)))
            }
            Err(TryRecvError::Empty) => (),
        }

        let queue = self.rx.shared.thread_queue();
        queue.push(cx.waker().clone());

        match self.rx.try_recv() {
            Ok(guard) => return Poll::Ready(Ok(guard)),
            Err(TryRecvError::Closed) => return Poll::Ready(Err(RecvError::Closed)),
            Err(TryRecvError::Lagged(skipped)) => {
                return Poll::Ready(Err(RecvError::Lagged(skipped)))
            }
            Err(TryRecvError::Empty) => Poll::Pending,
        }
    }
}
