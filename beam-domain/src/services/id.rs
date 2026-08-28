//! Identifier generation, abstracted so tests can assert on the identifiers a
//! flow produced rather than only that *some* identifier came back.

use uuid::Uuid;

/// A source of new entity identifiers.
pub trait IdGenerator: Send + Sync + std::fmt::Debug {
    fn new_id(&self) -> Uuid;
}

/// Production generator: random v4 UUIDs.
#[derive(Debug, Default, Clone)]
pub struct UuidGenerator;

impl IdGenerator for UuidGenerator {
    fn new_id(&self) -> Uuid {
        Uuid::new_v4()
    }
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build. See [`super::clock::in_memory`]
/// for why the `#[mutants::skip]` is required.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;

    /// Deterministic generator producing `00000000-0000-0000-0000-0000000000NN`
    /// for a monotonically increasing `NN`, so a test can name the identifier a
    /// flow will assign before it runs.
    #[derive(Debug, Default)]
    pub struct SequentialIdGenerator {
        next: std::sync::atomic::AtomicU64,
    }

    impl SequentialIdGenerator {
        pub fn new() -> Self {
            Self::default()
        }

        /// The identifier the `n`th (0-based) call to `new_id` will return.
        pub fn nth(n: u64) -> Uuid {
            Uuid::from_u64_pair(0, n + 1)
        }
    }

    impl IdGenerator for SequentialIdGenerator {
        fn new_id(&self) -> Uuid {
            let n = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Self::nth(n)
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::SequentialIdGenerator;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_production_generator_produces_distinct_non_nil_identifiers() {
        // A generator that returned a constant -- the nil UUID being the
        // likeliest accident -- would make every entity collide on its primary
        // key, which the type system cannot notice.
        let generator = UuidGenerator;
        let ids: HashSet<Uuid> = (0..1000).map(|_| generator.new_id()).collect();

        assert_eq!(ids.len(), 1000, "identifiers must not repeat");
        assert!(
            !ids.contains(&Uuid::nil()),
            "the nil UUID is not a usable identifier"
        );
        assert!(
            ids.iter().all(|id| id.get_version_num() == 4),
            "identifiers must be random v4, not derived from anything guessable"
        );
    }

    #[test]
    fn the_test_generator_hands_out_the_identifiers_it_promises() {
        // `nth` is what lets a test name an identifier before the flow runs;
        // if it disagreed with `new_id` the promise would be silently broken.
        let generator = SequentialIdGenerator::new();
        for n in 0..5 {
            assert_eq!(generator.new_id(), SequentialIdGenerator::nth(n));
        }
        assert_ne!(SequentialIdGenerator::nth(0), Uuid::nil());
    }
}
