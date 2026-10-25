//! The platform's synchronization primitives: `parking_lot` and the standard library's once-cells on a hosted build,
//! spin locks on a bare-metal one. Every caller names these paths, never a platform crate.

#[cfg(feature = "std")]
pub use parking_lot::{Condvar, Mutex, MutexGuard, RwLock};
#[cfg(feature = "std")]
pub use std::sync::{LazyLock, OnceLock};

#[cfg(not(feature = "std"))]
pub use spin::{Lazy as LazyLock, Mutex, MutexGuard, RwLock};

/// A cell written once, read many times; the bare-metal twin of `std::sync::OnceLock` with the same surface.
#[cfg(not(feature = "std"))]
pub struct OnceLock<T>(spin::Once<T>);

#[cfg(not(feature = "std"))]
impl<T> Default for OnceLock<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(feature = "std"))]
impl<T: Clone> Clone for OnceLock<T> {
    fn clone(&self) -> Self {
        let copy = Self::new();
        if let Some(value) = self.get() {
            let _ = copy.set(value.clone());
        }
        copy
    }
}

#[cfg(not(feature = "std"))]
impl<T: core::fmt::Debug> core::fmt::Debug for OnceLock<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("OnceLock").field(&self.get()).finish()
    }
}

#[cfg(not(feature = "std"))]
impl<T> OnceLock<T> {
    pub const fn new() -> Self {
        OnceLock(spin::Once::new())
    }

    pub fn get(&self) -> Option<&T> {
        self.0.get()
    }

    pub fn get_or_init(&self, init: impl FnOnce() -> T) -> &T {
        self.0.call_once(init)
    }

    pub fn set(&self, value: T) -> Result<(), T> {
        let mut offered = Some(value);
        self.0
            .call_once(|| offered.take().expect("the value is offered once"));
        match offered {
            None => Ok(()),
            Some(refused) => Err(refused),
        }
    }
}

/// Kernel-local state: `std::thread_local!` on a hosted build. A bare-metal build runs one kernel thread per address
/// space, so there it is a static read through the same `with`; the bare-metal host must keep to one thread.
#[cfg(feature = "std")]
#[macro_export]
macro_rules! kernel_local {
    ($(#[$meta:meta])* $vis:vis static $name:ident : $ty:ty = const { $init:expr }; $($rest:tt)*) => {
        ::std::thread_local! { $(#[$meta])* $vis static $name: $ty = const { $init }; }
        $crate::kernel_local!($($rest)*);
    };
    ($(#[$meta:meta])* $vis:vis static $name:ident : $ty:ty = $init:expr; $($rest:tt)*) => {
        ::std::thread_local! { $(#[$meta])* $vis static $name: $ty = $init; }
        $crate::kernel_local!($($rest)*);
    };
    () => {};
}

#[cfg(not(feature = "std"))]
#[macro_export]
macro_rules! kernel_local {
    ($(#[$meta:meta])* $vis:vis static $name:ident : $ty:ty = const { $init:expr }; $($rest:tt)*) => {
        $(#[$meta])* $vis static $name: $crate::sync::KernelLocal<$ty> = $crate::sync::KernelLocal::new(|| $init);
        $crate::kernel_local!($($rest)*);
    };
    ($(#[$meta:meta])* $vis:vis static $name:ident : $ty:ty = $init:expr; $($rest:tt)*) => {
        $(#[$meta])* $vis static $name: $crate::sync::KernelLocal<$ty> = $crate::sync::KernelLocal::new(|| $init);
        $crate::kernel_local!($($rest)*);
    };
    () => {};
}

/// The bare-metal [`kernel_local!`] cell: built on first use, read in place.
#[cfg(not(feature = "std"))]
pub struct KernelLocal<T>(spin::Lazy<T>);

// SAFETY: a bare-metal build runs exactly one kernel thread per address space (the documented host contract above),
// so the cell is never reached from two threads.
#[cfg(not(feature = "std"))]
unsafe impl<T> Sync for KernelLocal<T> {}

#[cfg(not(feature = "std"))]
impl<T> KernelLocal<T> {
    pub const fn new(init: fn() -> T) -> Self {
        KernelLocal(spin::Lazy::new(init))
    }

    pub fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.0)
    }
}

/// Which kernel thread is running: the standard library's thread id on a hosted build, the one kernel thread on bare
/// metal.
#[cfg(feature = "std")]
pub type ThreadKey = std::thread::ThreadId;

#[cfg(feature = "std")]
pub fn current_thread() -> ThreadKey {
    std::thread::current().id()
}

#[cfg(not(feature = "std"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadKey(());

#[cfg(not(feature = "std"))]
pub fn current_thread() -> ThreadKey {
    ThreadKey(())
}

/// The bare-metal stand-in for a condition variable. With one kernel thread nothing else can ever signal it, so a wait
/// is a host defect (a dispatch entered while another held the lane) and fails loudly instead of hanging.
#[cfg(not(feature = "std"))]
#[derive(Default)]
pub struct Condvar(());

#[cfg(not(feature = "std"))]
impl Condvar {
    pub const fn new() -> Self {
        Condvar(())
    }

    pub fn wait<T>(&self, _guard: &mut MutexGuard<'_, T>) {
        panic!("a bare-metal host runs one kernel thread: a dispatch waited for the lane another holds; resume the parked dispatch before entering a new one");
    }

    pub fn notify_all(&self) -> bool {
        false
    }

    pub fn notify_one(&self) -> bool {
        false
    }
}
