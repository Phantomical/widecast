use std::cell::UnsafeCell;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use thread_local::ThreadLocal;
use widecast::{Receiver, RecvError, Sender};

pub struct State {
    pub latencies: Histogram<u32>,
    pub send: Histogram<u32>,
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

        let snapshot = &self.send;
        let pct = |percentile| {
            let nanos = snapshot.value_at_percentile(percentile);
            humantime::format_duration(Duration::from_nanos(nanos.into()))
        };

        writeln!(f, "\nsend duration:")?;
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
    #[arg(long, default_value_t = 128)]
    pub capacity: usize,
    #[arg(long, default_value_t = 1)]
    pub chunk: usize,

    #[arg(long, default_value_t = 1)]
    pub delay: u64,
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

        let send_messages = rt.spawn(send_messages(tx, self));

        rt.block_on(async {
            for handle in handles {
                handle.await.expect("reader task panicked");
            }
        });
        let histogram = rt.block_on(send_messages).unwrap();
        rt.shutdown_timeout(Duration::from_secs(1));

        let locals = Arc::into_inner(histograms).unwrap();

        let mut total = make_histogram();
        for histogram in locals {
            total += histogram.into_inner();
        }

        State {
            latencies: total,
            send: histogram,
        }
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

async fn send_messages(mut tx: Sender<Instant>, config: Config) -> Histogram<u32> {
    let mut messages = Vec::with_capacity(config.chunk);
    let mut histogram = make_histogram();

    let period = Duration::from_millis(config.delay);
    let mut next = tokio::time::Instant::now() + period;

    for _ in 0..(config.messages / config.chunk) {
        let now = Instant::now();
        messages.resize(config.chunk, now);

        tx.send_bulk(messages.drain(..));

        let elapsed = now.elapsed();
        let _ = histogram.record(elapsed.as_nanos() as _);

        tokio::time::sleep_until(next).await;
        next += period;
    }

    histogram
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
