//! The one entropy source: a ChaCha20 DRBG seeded from the journaled `Random` ask. A session installs its DRBG
//! for the span of each call into rustls; outside that span there is no randomness and every draw fails.

use std::cell::RefCell;

use rand_chacha::rand_core::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use rustls::crypto::{GetRandomFailed, SecureRandom};

thread_local! {
    static ACTIVE: RefCell<Option<ChaCha20Rng>> = const { RefCell::new(None) };
}

pub fn drbg(seed: [u8; 32]) -> ChaCha20Rng {
    ChaCha20Rng::from_seed(seed)
}

pub struct Scope<'a> {
    home: &'a mut Option<ChaCha20Rng>,
}

impl<'a> Scope<'a> {
    pub fn enter(home: &'a mut Option<ChaCha20Rng>) -> Self {
        ACTIVE.with(|slot| *slot.borrow_mut() = home.take());
        Self { home }
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        ACTIVE.with(|slot| *self.home = slot.borrow_mut().take());
    }
}

pub fn with_rng<R>(f: impl FnOnce(&mut ChaCha20Rng) -> R) -> Result<R, GetRandomFailed> {
    ACTIVE.with(|slot| slot.borrow_mut().as_mut().map(f).ok_or(GetRandomFailed))
}

#[derive(Debug)]
pub struct SeededRandom;

impl SecureRandom for SeededRandom {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), GetRandomFailed> {
        with_rng(|rng| rng.fill_bytes(bytes))
    }
}
