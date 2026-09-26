//! Thread-local tracking for code inside a panic-containment boundary.
//! `contained` marks a call; it does not catch a panic itself. Callers pair it
//! with `catch_unwind`. The app's panic hook reads the marker to log contained
//! panics without writing a crash file.
//!
//! The hook runs on the panicking thread before unwinding drops the guard, so
//! it can observe the marker. A depth counter preserves it across nested calls;
//! the outermost guard clears it on return or unwind. Other threads are unaffected.

use std::cell::Cell;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Runs `f` with this thread marked "inside a containment boundary" for
/// the duration — including the instant a panic inside `f` reaches the
/// process panic hook, since the hook runs before any `Drop` (this
/// guard's included) executes as part of unwinding. Nested calls
/// compose: the marker clears only once the outermost `contained`
/// call's guard drops, i.e. once `f` (and everything it panicked
/// through) has fully returned or unwound.
pub fn contained<F: FnOnce() -> R, R>(f: F) -> R {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
    DEPTH.with(|d| d.set(d.get() + 1));
    let _guard = Guard;
    f()
}

/// True while this thread is somewhere inside a `contained` call —
/// including, critically, from inside the process panic hook while a
/// panic raised inside one is still unwinding out of it.
pub fn is_contained() -> bool {
    DEPTH.with(|d| d.get() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contained_reports_true_only_for_its_own_extent() {
        assert!(!is_contained());
        contained(|| {
            assert!(is_contained());
        });
        assert!(!is_contained());
    }

    #[test]
    fn nested_contained_calls_clear_only_once_the_outermost_returns() {
        contained(|| {
            assert!(is_contained());
            contained(|| {
                assert!(is_contained());
            });
            // The inner guard already dropped; the outer one hasn't.
            assert!(is_contained());
        });
        assert!(!is_contained());
    }

    #[test]
    fn the_marker_is_thread_local() {
        assert!(!is_contained());
        let handle = std::thread::spawn(|| {
            contained(|| {
                assert!(is_contained());
            });
        });
        // This thread was never marked, regardless of what the spawned
        // one is doing right now.
        assert!(!is_contained());
        handle.join().unwrap();
    }

    /// A caught panic clears the marker after unwinding. This exercises the
    /// `contained`/`catch_unwind` pairing without replacing the process-wide hook;
    /// the separate scope test checks the marker while the call is active.
    #[test]
    fn a_panic_inside_contained_stays_visible_through_the_unwind_and_clears_after() {
        assert!(!is_contained());
        let result = std::panic::catch_unwind(|| {
            contained(|| {
                assert!(is_contained());
                panic!("boom");
            })
        });
        assert!(result.is_err());
        assert!(!is_contained());
    }
}
