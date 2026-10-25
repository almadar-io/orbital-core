//! orbital-core's crates name `std::` paths. On a bare-metal build (no `std` feature) each crate aliases this crate as
//! `std`, so those paths resolve to `core`, `alloc` and `hashbrown` without a second spelling at every call site.
//! Only re-exports live here; anything the platform must supply (clock, entropy, locks) is `orbital-core-base`'s seam.
#![no_std]

extern crate alloc;

pub use alloc::format;
pub use alloc::{borrow, boxed, fmt, rc, slice, str, string, vec};
pub use core::{
    any, array, ascii, cell, char, clone, cmp, convert, default, error, f32, f64, future, hash,
    hint, iter, marker, mem, net, num, ops, option, panic, pin, primitive, ptr, result, task, time,
};
pub use core::{
    assert, assert_eq, assert_ne, debug_assert, debug_assert_eq, debug_assert_ne, matches, todo,
    unimplemented, unreachable, write, writeln,
};

pub mod collections {
    pub use alloc::collections::*;
    pub use hashbrown::{HashMap, HashSet};

    pub mod hash_map {
        pub use hashbrown::hash_map::*;
        pub use hashbrown::DefaultHashBuilder as RandomState;
    }

    pub mod hash_set {
        pub use hashbrown::hash_set::*;
    }
}

pub mod sync {
    pub use alloc::sync::{Arc, Weak};
    pub use core::sync::atomic;
}

pub mod prelude {
    pub mod v1 {
        pub use crate::RemEuclid;
        pub use alloc::borrow::ToOwned;
        pub use alloc::boxed::Box;
        pub use alloc::string::{String, ToString};
        pub use alloc::vec::Vec;
        pub use alloc::{format, vec};
        /// The float methods the standard library defines inherently (`fract`, `floor`, `sqrt`, …), from `libm`.
        pub use num_traits::Float;
    }
}

/// `f64::rem_euclid` / `f32::rem_euclid` with the standard library's definition, which `core` does not carry.
pub trait RemEuclid {
    fn rem_euclid(self, rhs: Self) -> Self;
}

impl RemEuclid for f64 {
    fn rem_euclid(self, rhs: f64) -> f64 {
        let r = self % rhs;
        if r < 0.0 {
            r + num_traits::Float::abs(rhs)
        } else {
            r
        }
    }
}

impl RemEuclid for f32 {
    fn rem_euclid(self, rhs: f32) -> f32 {
        let r = self % rhs;
        if r < 0.0 {
            r + num_traits::Float::abs(rhs)
        } else {
            r
        }
    }
}
