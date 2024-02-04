use std::ops::Deref;
use std::panic::Location;

use loom::sync::{Arc, RwLock};

pub(crate) struct ArcSwap<T> {
    cell: RwLock<Arc<T>>,
}

impl<T> ArcSwap<T> {
    pub fn new(value: Arc<T>) -> Self {
        Self {
            cell: RwLock::new(value),
        }
    }

    #[track_caller]
    pub fn load(&self) -> Guard<Arc<T>> {
        Guard(self.load_full())
    }

    #[track_caller]
    pub fn load_full(&self) -> Arc<T> {
        tracing::trace!(location = %Location::caller(), "ArcSwap::load");
        self.cell.read().unwrap().clone()
    }

    #[track_caller]
    pub fn store(&self, value: Arc<T>) {
        tracing::trace!(location = %Location::caller(), "ArcSwap::store");
        *self.cell.write().unwrap() = value;
    }
}

pub(crate) struct Guard<T>(T);

impl<T> Deref for Guard<Arc<T>> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
