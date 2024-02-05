//! A fast spsc queue

#![warn(unsafe_op_in_unsafe_fn)]

#[macro_use]
extern crate cfg_if;

mod channel;
mod detail;
mod raw;
mod shim;

pub use crate::channel::*;
pub use crate::detail::Drain;
pub use crate::raw::RawQueue;
