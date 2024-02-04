//! A fast spsc queue

#![warn(unsafe_op_in_unsafe_fn)]

#[macro_use]
extern crate cfg_if;

mod detail;
pub mod raw;
mod shim;

pub use crate::detail::Drain;

// use self::shim as sync;
