//! This is a shim layer to ensure that the rest of the crate can remain the
//! same when we use either loom or std.

cfg_if! {
    if #[cfg(loom)] {
        mod loom;

        pub(crate) use ::loom::cell::UnsafeCell;
        pub(crate) use ::loom::sync::atomic::AtomicU64;
        pub(crate) use ::loom::sync::Arc;

        pub(crate) use self::loom::{ArcSwap, Guard};
    } else {
        mod util;

        pub(crate) use std::sync::atomic::AtomicU64;
        pub(crate) use std::sync::Arc;
    
        pub(crate) use arc_swap::{ArcSwap, Guard};
    
        pub(crate) use self::util::UnsafeCell;
    }
}
