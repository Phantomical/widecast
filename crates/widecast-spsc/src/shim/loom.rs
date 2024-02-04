use std::ops::Deref;

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

    pub fn load(&self) -> Guard<Arc<T>> {
        Guard(self.load_full())
    }

    pub fn load_full(&self) -> Arc<T> {
        self.cell.read().unwrap().clone()
    }

    pub fn store(&self, value: Arc<T>) {
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
