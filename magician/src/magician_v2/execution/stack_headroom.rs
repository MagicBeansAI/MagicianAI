//! Stack headroom for deep work polled from a deep caller.
//!
//! Execution workers run on tokio's default 2 MiB stack (see
//! `runtime_boundary`; raising it is deliberately refused). Chat's turn
//! handling alone holds about 1.4 MiB of poll frames in a debug build, and a
//! tool call reached from it can need another ~0.7 MiB (content search →
//! retrieval ladder → discovery → governed runtime). Measured from the
//! 2026-10-11 crash reports: 2,121,680 bytes on a 2 MiB stack.
//!
//! [`with_stack_headroom`] wraps such a future: each poll runs through
//! `stacker::maybe_grow`, so when less than [`RED_ZONE`] of the current stack
//! remains, that poll runs on a fresh [`SEGMENT`]-sized stack. A poll is
//! synchronous, so no stack switch ever spans an await; the future itself is
//! heap-pinned, so nothing it holds lives on the temporary segment.
//!
//! The red zone IS the segment, so a wrapped poll always has a whole segment
//! below it. A smaller red zone was a guess about how deep the work goes: at
//! 512 KiB, a chat `list_proposals` call (harness provider `execute`) entered
//! with more than 512 KiB left, ran inline, and overflowed the 2 MiB worker
//! (2026-10-12 crash report: `WithStackHeadroom` on the stack, no segment
//! below it). On a 2 MiB worker every wrapped poll therefore takes a fresh
//! segment; it is mapped lazily, so only the pages a poll touches are used.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Size of a fresh segment.
pub const SEGMENT: usize = 8 * 1024 * 1024;

/// Remaining stack below which a poll moves to a fresh segment: the whole
/// segment, so a wrapped poll never starts with less than [`SEGMENT`].
pub const RED_ZONE: usize = SEGMENT;

/// A future whose every poll has at least [`RED_ZONE`] of stack.
pub struct WithStackHeadroom<F: Future> {
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for WithStackHeadroom<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let inner = &mut self.inner;
        stacker::maybe_grow(RED_ZONE, SEGMENT, || inner.as_mut().poll(cx))
    }
}

/// Run `future` with stack headroom on every poll (heap-pinned as well, so a
/// caller's state machine does not carry it inline).
pub fn with_stack_headroom<F: Future>(future: F) -> WithStackHeadroom<F> {
    WithStackHeadroom {
        inner: Box::pin(future),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Burns about `depth` KiB of stack in plain recursion.
    #[inline(never)]
    fn burn(depth: usize) -> usize {
        let pad = std::hint::black_box([0u8; 1024]);
        if depth == 0 {
            return pad[0] as usize;
        }
        burn(depth - 1) + pad[depth % 1024] as usize
    }

    /// On a 256 KiB thread, a poll that needs ~1 MiB of stack completes when
    /// wrapped: the poll moves to a fresh segment instead of overflowing.
    #[test]
    fn a_deep_poll_on_a_small_stack_gets_a_fresh_segment() {
        let handle = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                runtime.block_on(with_stack_headroom(async { burn(1024) }))
            })
            .unwrap();
        assert_eq!(handle.join().expect("no overflow"), 0);
    }

    /// The 2026-10-12 crash: the caller already used most of a 2 MiB worker
    /// but more than the old 512 KiB red zone remained, so the poll ran
    /// inline and overflowed. A wrapped poll must start with a whole segment
    /// whatever the caller left, and survive work deeper than the worker.
    #[test]
    fn a_wrapped_poll_always_starts_with_a_whole_segment() {
        fn deep_caller(depth: usize) -> usize {
            let pad = std::hint::black_box([0u8; 1024]);
            if depth > 0 {
                return deep_caller(depth - 1) + pad[depth % 1024] as usize;
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(with_stack_headroom(async {
                let remaining = stacker::remaining_stack().unwrap_or(0);
                assert!(
                    remaining >= SEGMENT - 256 * 1024,
                    "a wrapped poll started with {remaining} bytes, less than a segment"
                );
                burn(3 * 1024)
            }))
        }
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            // About half the worker used before the wrapper (debug frames
            // are ~2 KiB here): more than the old 512 KiB red zone left, far
            // less than a segment — the 2026-10-12 crash's starting point.
            .spawn(|| deep_caller(512))
            .unwrap();
        assert_eq!(handle.join().expect("no overflow"), 0);
    }

    #[test]
    fn it_returns_the_wrapped_output() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(runtime.block_on(with_stack_headroom(async { 41 + 1 })), 42);
    }
}
