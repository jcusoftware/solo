//! A FIFO async mutex for the Durable Object (pure, host-testable).
//!
//! Message handlers await D1 (membership rechecks) before their synchronous
//! commit. The runtime starts each inbound WebSocket message's handler in
//! arrival order, but without serialization a later message whose awaits finish
//! sooner could commit first. [`Fifo`] hands out tickets in call order and lets
//! exactly one holder run at a time, so message handling is applied in the
//! order messages arrived (per socket, and across sockets of one object).
//!
//! Single-threaded by design (`Rc`/`Cell`): the Workers runtime runs one
//! isolate thread. A waiter that is dropped before its turn (a cancelled
//! handler) is skipped, so a cancelled future can never wedge the queue.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

#[derive(Default)]
struct Inner {
    next_ticket: Cell<u64>,
    serving: Cell<u64>,
    /// Tickets that gave up before being served.
    abandoned: RefCell<BTreeSet<u64>>,
    wakers: RefCell<Vec<Waker>>,
}

impl Inner {
    /// Advance past the served ticket and any abandoned successors, then wake waiters.
    fn advance(&self) {
        self.serving.set(self.serving.get() + 1);
        loop {
            let s = self.serving.get();
            if self.abandoned.borrow_mut().remove(&s) {
                self.serving.set(s + 1);
            } else {
                break;
            }
        }
        for w in self.wakers.borrow_mut().drain(..) {
            w.wake();
        }
    }
}

#[derive(Clone, Default)]
pub struct Fifo {
    inner: Rc<Inner>,
}

impl Fifo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the next ticket NOW (call this before the first `.await` of a
    /// handler); await the returned future for the turn.
    pub fn enter(&self) -> Turn {
        let t = self.inner.next_ticket.get();
        self.inner.next_ticket.set(t + 1);
        Turn {
            inner: self.inner.clone(),
            ticket: t,
            state: TurnState::Waiting,
        }
    }

    /// True when `max` callers are already holding or waiting: the caller should
    /// refuse the message instead of calling [`Fifo::enter`] (bounded queue).
    pub fn is_full(&self, max: u64) -> bool {
        self.depth() >= max
    }

    /// Callers currently holding or waiting for a turn.
    pub fn depth(&self) -> u64 {
        self.inner.next_ticket.get() - self.inner.serving.get()
    }
}

#[derive(PartialEq, Eq)]
enum TurnState {
    Waiting,
    Held,
}

/// A ticket. Resolves to a [`Held`] guard when it is this ticket's turn.
pub struct Turn {
    inner: Rc<Inner>,
    ticket: u64,
    state: TurnState,
}

/// Dropping the guard releases the lock to the next ticket.
pub struct Held {
    inner: Rc<Inner>,
}

impl Future for Turn {
    type Output = Held;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Held> {
        if self.inner.serving.get() == self.ticket {
            self.state = TurnState::Held;
            return Poll::Ready(Held {
                inner: self.inner.clone(),
            });
        }
        self.inner.wakers.borrow_mut().push(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        match self.state {
            // Never served: make sure the queue skips this ticket.
            TurnState::Waiting => {
                if self.inner.serving.get() == self.ticket {
                    // It was this ticket's turn but it never polled: release.
                    self.inner.advance();
                } else {
                    self.inner.abandoned.borrow_mut().insert(self.ticket);
                }
            }
            TurnState::Held => {}
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.inner.advance();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll_once<F: Future + Unpin>(f: &mut F) -> Poll<F::Output> {
        let mut cx = Context::from_waker(Waker::noop());
        Pin::new(f).poll(&mut cx)
    }

    #[test]
    fn grants_turns_in_ticket_order_even_when_polled_out_of_order() {
        let f = Fifo::new();
        let mut a = f.enter();
        let mut b = f.enter();
        let mut c = f.enter();
        assert_eq!(f.depth(), 3);
        // c and b poll first but must wait for a.
        assert!(poll_once(&mut c).is_pending());
        assert!(poll_once(&mut b).is_pending());
        let Poll::Ready(ga) = poll_once(&mut a) else {
            panic!("a is first")
        };
        assert!(poll_once(&mut b).is_pending(), "a still holds the turn");
        drop(ga);
        assert!(poll_once(&mut c).is_pending(), "b is next, not c");
        let Poll::Ready(gb) = poll_once(&mut b) else {
            panic!("b is next")
        };
        drop(gb);
        assert!(poll_once(&mut c).is_ready());
    }

    #[test]
    fn the_queue_is_bounded_by_the_caller_checking_is_full_before_entering() {
        let f = Fifo::new();
        let mut turns: Vec<Turn> = Vec::new();
        let mut refused = 0;
        for _ in 0..50 {
            if f.is_full(crate::session::MAX_QUEUED_MESSAGES) {
                refused += 1; // the object answers rate_limited and does not enter
            } else {
                turns.push(f.enter());
            }
        }
        assert_eq!(turns.len() as u64, crate::session::MAX_QUEUED_MESSAGES);
        assert_eq!(refused, 50 - turns.len());
        assert_eq!(f.depth(), crate::session::MAX_QUEUED_MESSAGES);
        // Finishing one frees exactly one slot.
        let mut first = turns.remove(0);
        let Poll::Ready(held) = poll_once(&mut first) else {
            panic!()
        };
        assert!(f.is_full(crate::session::MAX_QUEUED_MESSAGES));
        drop(held);
        assert!(!f.is_full(crate::session::MAX_QUEUED_MESSAGES));
    }

    #[test]
    fn a_cancelled_waiter_does_not_wedge_the_queue() {
        let f = Fifo::new();
        let mut a = f.enter();
        let b = f.enter();
        let mut c = f.enter();
        let Poll::Ready(ga) = poll_once(&mut a) else {
            panic!()
        };
        drop(b); // cancelled while waiting
        assert!(poll_once(&mut c).is_pending());
        drop(ga);
        assert!(poll_once(&mut c).is_ready(), "b was skipped");
        assert_eq!(
            f.depth(),
            0,
            "the abandoned ticket was skipped, c ran and released"
        );
    }

    #[test]
    fn a_ticket_dropped_at_the_head_releases_the_next() {
        let f = Fifo::new();
        let a = f.enter();
        let mut b = f.enter();
        drop(a); // never polled, but it was its turn
        assert!(poll_once(&mut b).is_ready());
    }
}
