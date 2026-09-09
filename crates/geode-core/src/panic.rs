//! Panic containment tracking (Phase 4b Task 6 fix round 1, MAJ-1): a
//! thread-local marker set while code runs inside one of this
//! codebase's own `catch_unwind` boundaries (the ingest load, the
//! ingest runner's pop-time catalog recheck, a discovery poll, a query
//! pool worker), read by the process panic hook
//! (`geode_app::crash::install_panic_hook`) to tell a *contained*
//! panic — one of those boundaries doing exactly what it is for — from
//! an *uncontained* one that is genuinely taking the process down. A
//! contained panic is not a crash: the hook logs it and writes no
//! file.
//!
//! A depth counter, not a bool: `contained` calls can nest (a boundary
//! calling into code that itself opens another one), and only the
//! outermost guard's drop should clear the marker. Thread-local because
//! a process panic hook always runs on the panicking thread itself
//! (Rust panic semantics: the hook fires *before* unwinding begins, on
//! the same thread `panic!` was called from), so a guard entered on the
//! ingest thread must stay invisible to a panic on, say, the query
//! pool's own thread.

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

    /// The property the panic hook's decision actually depends on
    /// (fix round 1, MAJ-1's own instruction: "test the decision
    /// function, not the global hook" — no `std::panic::set_hook` here,
    /// just the same `contained`/`catch_unwind` pairing every real
    /// boundary uses): a panic raised inside `contained`, caught by
    /// `catch_unwind` the way this codebase's four boundaries wrap it,
    /// is visible to `is_contained()` for as long as it is unwinding
    /// through the guard's frame — which is exactly when the panic hook
    /// runs — and clears once `catch_unwind` has returned.
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
