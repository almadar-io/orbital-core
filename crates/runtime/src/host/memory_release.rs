//! Returning freed heap pages to the OS once a host has registered its program: boot builds the program in several
//! transient forms, and the allocator otherwise keeps those freed pages mapped for the life of the process.

#[cfg(target_os = "macos")]
extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut libc::malloc_zone_t, goal: libc::size_t) -> libc::size_t;
}

/// Ask the allocator to give every freed page back to the OS. Answers the bytes released where the allocator reports
/// them (macOS), else `None`.
pub fn release_freed_memory() -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        // A null zone relieves every zone; a goal of 0 asks for as much as can be released.
        Some(unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) })
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe { libc::malloc_trim(0) };
        None
    }
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::release_freed_memory;

    #[test]
    fn freed_pages_go_back_after_a_large_transient_allocation() {
        let transient: Vec<Vec<u8>> = (0..64).map(|_| vec![1u8; 1 << 20]).collect();
        drop(transient);
        let released = release_freed_memory();
        #[cfg(target_os = "macos")]
        assert!(released.is_some());
        #[cfg(not(target_os = "macos"))]
        assert!(released.is_none());
    }

    #[test]
    fn control_releasing_twice_is_harmless() {
        release_freed_memory();
        release_freed_memory();
    }
}
