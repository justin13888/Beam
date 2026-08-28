//! Cross-cutting outbound seams shared by every crate above `beam-domain`.
//!
//! Each concern has exactly one canonical trait here; new code reaches for the
//! existing seam rather than introducing a second abstraction for the same
//! thing. See the seam catalog in `AGENTS.md`.

pub mod clock;
pub mod id;

pub use clock::{Clock, RealClock};
pub use id::{IdGenerator, UuidGenerator};

#[cfg(any(test, feature = "test-utils"))]
pub use clock::TestClock;
#[cfg(any(test, feature = "test-utils"))]
pub use id::SequentialIdGenerator;
