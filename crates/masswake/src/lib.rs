use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Waker};

use crossbeam_utils::CachePadded;
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
}

impl Interest {
    pub fn new(notify: &MassNotify) -> Self {
        Self {
            shared: notify.shared.clone(),
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

        // SAFETY: This is only ever called on the queue for the local thread.
        unsafe { local.queue.push_local(cx.waker().clone()) };
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
