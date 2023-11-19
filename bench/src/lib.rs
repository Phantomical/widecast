use std::cell::UnsafeCell;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use thread_local::ThreadLocal;
use widecast::{Receiver, RecvError, Sender};

pub struct State {
    pub latencies: Histogram<u32>,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = &self.latencies;
        let pct = |percentile| {
            let nanos = snapshot.value_at_percentile(percentile);
            humantime::format_duration(Duration::from_nanos(nanos.into()))
        };

        writeln!(f, "queue delay:")?;
        writeln!(f, "  p50:  {}", pct(50.0))?;
        writeln!(f, "  p90:  {}", pct(90.0))?;
        writeln!(f, "  p95:  {}", pct(95.0))?;
        writeln!(f, "  p99:  {}", pct(99.0))?;
        writeln!(f, "  p999: {}", pct(99.9))?;

        Ok(())
    }
}

#[derive(Copy, Clone, Debug, clap::Parser)]
pub struct Config {
    #[arg(long)]
    pub readers: usize,
    #[arg(long)]
    pub messages: usize,
    #[arg(long)]
    pub capacity: usize,
    #[arg(long)]
    pub chunk: usize,
}

impl Config {
    pub fn test_loaded_latency(self) -> State {
        let tx = Sender::<Instant>::new(self.capacity);
        let histograms = Arc::new(ThreadLocal::new());

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");
        let mut handles = Vec::new();

        for _ in 0..self.readers {
            handles.push(rt.spawn(consume_messages(tx.subscribe(), histograms.clone())));
        }

        handles.push(rt.spawn(send_messages(tx, self)));

        rt.block_on(async {
            for handle in handles {
                handle.await.expect("reader task panicked");
            }
        });
        rt.shutdown_timeout(Duration::from_secs(1));

        let locals = Arc::into_inner(histograms).unwrap();

        let mut total = make_histogram();
        for histogram in locals {
            total += histogram.into_inner();
        }

        State { latencies: total }
    }
}

async fn consume_messages(mut rx: Receiver<Instant>, latencies: Arc<ThreadLocal<LocalHistogram>>) {
    loop {
        match rx.recv().await {
            Ok(ts) => {
                let since = ts.elapsed();

                let latencies = latencies.get_or(LocalHistogram::new);
                let _ = latencies.record(since.as_nanos() as u64);
            }
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
    }
}

async fn send_messages(mut tx: Sender<Instant>, config: Config) {
    let mut messages = Vec::with_capacity(config.chunk);

    for _ in 0..(config.messages / config.chunk) {
        let now = Instant::now();
        messages.resize(config.chunk, now);

        tx.send_bulk(messages.drain(..));

        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn make_histogram() -> Histogram<u32> {
    Histogram::new_with_bounds(1, Duration::from_secs(3600).as_nanos() as u64, 3).unwrap()
}

struct LocalHistogram(UnsafeCell<Histogram<u32>>);

impl LocalHistogram {
    pub fn new() -> Self {
        Self(UnsafeCell::new(make_histogram()))
    }

    pub fn record(&self, value: u64) {
        let _ = unsafe { &mut *self.0.get() }.record(value);
    }

    pub fn into_inner(self) -> Histogram<u32> {
        self.0.into_inner()
    }
}

unsafe impl Send for LocalHistogram {}
