//! A waker designed for efficiently waking a large number of tasks.
//!
//! This is meant to be used as a building block when writing your own futures.
//! All it does is efficiently keep track of wakers for all registered tasks
//! and notify them when [`MassNotify::notify_all`] is called.
//!
//! If you are using this to build bigger data structures then you will need an
//! external way to track whether the future is ready once it wakes up.
//!
//! # Example
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! use masswake::{MassNotify, Interest};
//! use std::task::Poll;
//! use std::time::Duration;
//!
//! let mut waker = MassNotify::new();
//! let mut interest = waker.register();
//!
//! let handle = tokio::spawn(async move {
//!     let mut done = false;
//!     std::future::poll_fn(|cx| {
//!         if !done {
//!             interest.register(cx);
//!             done = true;
//!             return Poll::Pending;
//!         }
//!
//!         Poll::Ready(())
//!     }).await;
//! });
//!
//! // Let the future run first so that it becomes registered.
//! tokio::time::sleep(Duration::from_millis(50)).await;
//!
//! // Now notify all the tasks.
//! waker.notify_all();
//! let _ = handle.await;
//! # }
//! ```
//!
//! # Limitations
//! - [`MassNotify`] maintains a SPSC queue per thread so having [`Interest`]s
//!   scattered across a large number of threads will result in a slow down.
//!   Note that queues can be reused once their owning thread exits so the issue
//!   really only shows up when a large number of threads are used concurrently.
//!
//! # How it Works
//! The main part of this crate is [`MassNotify`]. It maintains a set of SPSC
//! queues, one per thread. When an [`Interest`] registers itself, it adds a
//! waker to the local queue. On the other side, when [`MassNotify::notify_all`]
//! is called it removes all the wakers from each queue in bulk.
//!
//! The advantages of this approach are:
//! - All inserts happen on a local queue and so are pretty much uncontended.
//!   They still have to be atomic since `notify_all` needs to look at them but
//!   that happens infrequently in comparison.
//! - `notify_all` can spend most of its time iterating over an array of wakers
//!   instead of performing atomic queue operations.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[allow(unused_imports)] // used in docs
use std::task::Poll;
use std::task::{Context, Waker};

use crossbeam_utils::CachePadded;
use noop_waker::noop_waker;
use thread_local::ThreadLocal;

use crate::queue::LocalQueue;

mod queue;

type WakerQueue = LocalQueue<Waker>;

/// Notify a set of tasks to wake up.
///
/// [`MassNotify`] is analogous to tokio's [`Notify`] except that it is
/// restricted to only notify all available tasks. It is designed to be much
/// more efficient than [`Notify`] when working with a large number of tasks to
/// be woken (100 to 100k+).
///
/// To use this in a future, call [`register`] to create an [`Interest`] and
/// then call [`Interest::register`] from your future's `poll` method when you
/// would return [`Poll::Pending`].
///
/// [`Notify`]: https://docs.rs/tokio/1/tokio/sync/struct.Notify.html
/// [`register`]: MassNotify::register
#[derive(Default)]
pub struct MassNotify {
    shared: Arc<Shared>,
    mutex: Mutex<()>,
}

impl MassNotify {
    /// Create a new `MassNotify` with no registered [`Interest`]s.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wake all tasks that are currently awaiting a notification.
    ///
    /// Note that this takes a lock for the duration of the call. If you find
    /// that you are spending a lot of time within `notify_all` you may want to
    /// consider batching your events in order to send fewer notifications.
    pub fn notify_all(&self) {
        let _guard = self.mutex.lock().map_err(|e| e.into_inner());
        let mut queues = Vec::with_capacity(16);

        let this_thread = self.shared.locals.get().map(|local| {
            let watermark = local.update_watermark();
            (watermark, &local.queue)
        });

        for local in &self.shared.locals {
            let watermark = local.update_watermark();

            // SAFETY: We are the remote thread. The mutex ensures that this method is not
            //         called concurrently.
            if let Some(iter) = unsafe { local.queue.consume_remote(16, watermark) } {
                for data in iter {
                    data.wake();
                }
            }

            queues.push((watermark, &local.queue));
        }

        if let Some((_, local_queue)) = this_thread {
            queues.retain(|(_, queue)| {
                *queue as *const WakerQueue != local_queue as *const WakerQueue
            });
        }

        for (watermark, queue) in queues {
            // SAFETY: We are the remote thread. The mutex ensures that this method is not
            //         called concurrently.
            while let Some(iter) = unsafe { queue.consume_remote(128, watermark) } {
                for data in iter {
                    data.wake();
                }
            }
        }

        if let Some((watermark, queue)) = this_thread {
            if let Some(iter) = unsafe { queue.consume_remote(u64::MAX, watermark) } {
                for data in iter {
                    data.wake();
                }
            }
        }
    }

    /// Create a new [`Interest`] that can be used to have a task get
    /// notifications when [`notify_all`](MassNotify::notify_all) is called.
    pub fn register(&self) -> Interest {
        Interest::new(self)
    }
}

/// Listen for notifications from a [`MassNotify`].
///
/// This is analogous to tokio's [`Notified`] but is used differently because it
/// can be reused multiple times for many notifications.
///
/// To use, call [`register`] when your future would return [`Poll::Pending`].
///
/// [`Notified`]: https://docs.rs/tokio/1/tokio/sync/futures/struct.Notified.html
/// [`register`]: Interest::register
pub struct Interest {
    shared: Arc<Shared>,

    /// The watermark when `waker` was last pushed into the queue.
    watermark: u64,

    /// A reference to the watermark of the last queue we inserted a waker into.
    ///
    /// This is not `'static` but instead refers to `shared`. Do NOT touch it
    /// during drop.
    queue: Option<&'static AtomicU64>,

    /// The last waker to have been pushed into the queue.
    waker: Waker,
}

impl Interest {
    pub fn new(notify: &MassNotify) -> Self {
        Self {
            shared: notify.shared.clone(),
            watermark: u64::MAX,
            queue: None,
            waker: noop_waker(),
        }
    }

    /// Assist the notifier in waking up further tasks.
    ///
    /// By specifying `limit` you can limit the amount of work that the current
    /// assisting task will do. If you want to always wake all tasks that are
    /// scheduled to be woken up then use `u64::MAX`.
    ///
    /// Having woken tasks assist can reduce the amount of time spent in
    /// [`MassNotify::notify_all`] at the expense of spending more time when
    /// waking up tasks.
    ///
    /// ## A note on performance
    /// While at first glance it would seem this should usually give a
    /// performance and latency benefit, that is not always the case. Some
    /// executors (i.e. tokio) have different behaviours when a task is woken on
    /// the local thread vs a remote one. Be sure to measure the impact on
    /// performance and latency before using this.
    ///
    /// For tokio specifically, locals task wakeups are queued in LIFO order
    /// whereas remote wakeups are added in FIFO order. Under high load this may
    /// result in starvation as new tasks are queued before the tokio runtime
    /// can work through the whole queue.
    pub fn assist(&mut self, limit: u64) {
        let local = self.shared.thread_local();
        let watermark = local.watermark.load(Ordering::Acquire);

        // Assist the notifier with waking tasks. We need to be careful to only
        // wake tasks up to the current watermark to prevent tasks from starving
        // when new tasks get pushed in front of them.
        //
        // SAFETY: This is only ever called on the queue for the local thread.
        if let Some(iter) = unsafe { local.queue.consume_local(limit, watermark) } {
            for data in iter {
                data.wake();
            }
        }
    }

    /// Register the current task to be awoken the the next time the
    /// [`MassNotify::notify_all`] is called.
    pub fn register(&mut self, cx: &mut Context<'_>) {
        if !self.needs_to_register(cx) {
            return;
        }

        let local = self.shared.thread_local();

        // SAFETY: This is only ever called on the queue for the local thread.
        let index = unsafe { local.queue.push_local(cx.waker().clone()) };

        self.watermark = index;
        self.waker = cx.waker().clone();

        // SAFETY: `local` will live as long as the `shared` arc does.
        self.queue = unsafe { Some(&*(&local.watermark as *const _)) };
    }

    /// In certain cases we can avoid adding a new waker to the queue.
    ///
    /// This is needed to avoid unbounded memory usage in certain common use
    /// cases like `tokio::select!`.
    fn needs_to_register(&self, cx: &mut Context<'_>) -> bool {
        // Special case: u64::MAX means that we haven't ever registered a waker.
        if self.watermark == u64::MAX {
            return true;
        }

        // We may have migrated between threads since the last call to register. To work
        // around that we keep a reference to the watermark of the last queue that we
        // pushed a value into.
        let watermark = match self.queue {
            Some(queue) => queue.load(Ordering::Acquire),
            None => return true,
        };

        // We last registered before the last wake so we need to re-register.
        if self.watermark < watermark {
            return true;
        }

        // If the waker has changed then it is also necessary to re-register.
        //
        // Rust's waker design doesn't guarantee that we'll be woken if we don't wake
        // the waker from the latest context.
        !cx.waker().will_wake(&self.waker)
    }
}

struct Local {
    queue: WakerQueue,
    watermark: AtomicU64,
}

impl Local {
    fn new() -> Self {
        Self {
            queue: WakerQueue::with_capacity(1024),
            watermark: AtomicU64::new(0),
        }
    }

    fn update_watermark(&self) -> u64 {
        let watermark = self.queue.watermark();
        self.watermark.store(watermark, Ordering::Release);
        watermark
    }
}

#[derive(Default)]
struct Shared {
    locals: ThreadLocal<CachePadded<Local>>,
}

impl Shared {
    fn thread_local(&self) -> &Local {
        self.locals.get_or(|| CachePadded::new(Local::new()))
    }
}
