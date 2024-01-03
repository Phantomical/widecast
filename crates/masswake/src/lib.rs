use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Waker};
use std::thread::ThreadId;

use crossbeam_utils::CachePadded;
use noop_waker::noop_waker;
use thread_local::ThreadLocal;

use crate::queue::LocalQueue;

mod queue;

type WakerQueue = LocalQueue<Waker>;

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
    pub assist: bool,
}

///
#[derive(Default)]
pub struct MassNotify {
    shared: Arc<Shared>,
    mutex: Mutex<()>,
}

impl MassNotify {
    pub fn new() -> Self {
        Self::default()
    }

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
}

pub struct Interest {
    shared: Arc<Shared>,

    /// The watermark when `waker` was last pushed into the queue.
    watermark: u64,

    /// The thread that we were last registered on.
    thread_id: Option<ThreadId>,

    /// The last waker to have been pushed into the queue.
    waker: Waker,
}

impl Interest {
    pub fn new(notify: &MassNotify) -> Self {
        Self {
            shared: notify.shared.clone(),
            watermark: u64::MAX,
            thread_id: None,
            waker: noop_waker(),
        }
    }

    /// Assist the notifier in waking up further tasks.
    pub fn assist(&mut self) {
        let local = self.shared.thread_local();
        let watermark = local.watermark.load(Ordering::Acquire);

        // Assist the notifier with waking tasks. We need to be careful to only
        // wake tasks up to the current watermark to prevent tasks from starving
        // when new tasks get pushed in front of them.
        //
        // SAFETY: This is only ever called on the queue for the local thread.
        if let Some(iter) = unsafe { local.queue.consume_local(0, watermark) } {
            for data in iter {
                data.wake();
            }
        }
    }

    /// Register the current task to be awoken the the next time the
    /// [`MassNotify::notify_all`] is called.
    pub fn register(&mut self, cx: &mut Context<'_>) {
        let local = self.shared.thread_local();
        let current = local.watermark.load(Ordering::Acquire);

        if !self.needs_to_register(current, cx) {
            return;
        }

        // SAFETY: This is only ever called on the queue for the local thread.
        let index = unsafe { local.queue.push_local(cx.waker().clone()) };

        self.watermark = index;
        self.waker = cx.waker().clone();
        self.thread_id = Some(std::thread::current().id());
    }

    /// In certain cases we can avoid adding a new waker to the queue.
    ///
    /// This is needed to avoid unbounded memory usage in certain common use
    /// cases like `tokio::select!`.
    fn needs_to_register(&self, watermark: u64, cx: &mut Context<'_>) -> bool {
        // Special case: u64::MAX means that we haven't ever registered a waker.
        if self.watermark == u64::MAX {
            return true;
        }

        // If we have migrated between threads since the last register then we have no
        // way of telling if we need to register again so we need to re-register.
        match self.thread_id {
            None => return true,
            Some(id) if id != std::thread::current().id() => return true,
            _ => (),
        }

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
