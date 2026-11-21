//! One runtime namespace for model identities. Never persisted or copied by Clone.

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next() -> u64 {
    allocate(&NEXT)
}

pub(super) fn allocate(counter: &AtomicU64) -> u64 {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1).filter(|_| next != 0)
        })
        .unwrap_or_else(|_| panic!("docking runtime identity exhausted"))
}
