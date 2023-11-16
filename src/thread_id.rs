use std::hash::{Hash, Hasher};

use fxhash::FxHasher32;

thread_local! {
    /// We use the address of this value to uniquely determine the current
    /// thread. We only want a unique value to reduce contention between threads
    /// so this works fine.
    static THREAD_ID: u8 = 0;
}

pub(crate) fn thread_id() -> u64 {
    // Use FxHasher to ensure that thread ids are usefully distributed.

    let mut hasher = FxHasher32::default();
    THREAD_ID.with(|value| value as *const u8).hash(&mut hasher);
    hasher.finish()
}
