//! The transactional wrapper around one engine call (host-testable).
//!
//! The Durable Object runs every engine call inside `storage.transactionSync`.
//! Which of "committed" and "rolled back" happened must be decided from facts,
//! never from a flag that an earlier, unrelated call may have left behind:
//!
//! * the store failure flag is cleared **at the top of the transaction body**, so
//!   a failed read made outside a transaction cannot roll back a later success;
//! * a `transactionSync` that itself throws (not just a body that asked for a
//!   rollback) counts as failure, whatever the flag says;
//! * on any failure the cached engine is dropped and the caller gets `Err`, never
//!   the closure's result: the client must not be told "applied" for work the
//!   storage transaction discarded, and memory is never trusted again.

use std::cell::RefCell;

use super::engine::Engine;
use super::store::Store;

/// The transaction did not commit; the cached engine was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxFailed;

impl std::fmt::Display for TxFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session storage transaction failed and was rolled back")
    }
}

/// Run `f` on the cached engine inside `runner` (the `transactionSync` call).
/// `runner` calls the body it is given exactly once and returns `Err` if the
/// transaction did not commit (the body returned `Err`, or the call itself threw).
pub fn run_in_tx<S: Store, T>(
    cell: &RefCell<Option<Engine<S>>>,
    f: impl FnOnce(&mut Engine<S>) -> T,
    runner: impl FnOnce(&mut dyn FnMut() -> Result<(), ()>) -> Result<(), ()>,
) -> Result<T, TxFailed> {
    let mut f = Some(f);
    let mut out: Option<T> = None;
    let mut body_failed = false;
    let mut body = || -> Result<(), ()> {
        let mut guard = cell.borrow_mut();
        let Some(engine) = guard.as_mut() else {
            body_failed = true;
            return Err(());
        };
        // Forget a failure recorded by an earlier call outside this transaction.
        let _ = engine.store_failed();
        if let Some(f) = f.take() {
            out = Some(f(engine));
        }
        if engine.store_failed() {
            body_failed = true;
            return Err(());
        }
        Ok(())
    };
    let committed = runner(&mut body).is_ok();
    match out {
        Some(v) if committed && !body_failed => Ok(v),
        _ => {
            *cell.borrow_mut() = None;
            Err(TxFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::engine::{Actor, Rejection};
    use crate::session::store::MemStore;
    use sws_ir::edit::{Command, CommandBatch};

    const FIXTURE: &str = include_str!("../../tests/fixtures/session-site.json");

    fn text(v: &str) -> CommandBatch {
        CommandBatch::new(vec![Command::SetText {
            uid: "home.title-text".into(),
            value: v.into(),
        }])
    }

    fn actor() -> Actor {
        Actor {
            user_id: 1,
            client_id: "c-1".into(),
        }
    }

    fn cell() -> RefCell<Option<Engine<MemStore>>> {
        let mut e = Engine::open(MemStore::default(), "ws-a", "site-a1").unwrap();
        e.seed(FIXTURE, 0).unwrap();
        RefCell::new(Some(e))
    }

    /// A transactionSync stand-in that commits (the body's Ok) or reports a rollback.
    fn commit(body: &mut dyn FnMut() -> Result<(), ()>) -> Result<(), ()> {
        body()
    }

    #[test]
    fn a_stale_failure_flag_from_an_earlier_read_does_not_roll_back_a_later_success() {
        let c = cell();
        // An earlier read outside any transaction hit a storage error and left the flag set.
        c.borrow_mut().as_mut().unwrap().store_mut().failed = true;
        let ack = run_in_tx(
            &c,
            |e| e.submit_batch(&actor(), "op-00000001", 0, &text("A"), 1),
            commit,
        )
        .expect("the stale flag must not fail this transaction");
        assert_eq!(ack.unwrap().seq, 1);
        assert!(c.borrow().is_some(), "the engine was kept");
        assert_eq!(c.borrow().as_ref().unwrap().head_seq(), 1);
    }

    #[test]
    fn a_failure_inside_the_body_returns_err_not_the_closures_result_and_drops_the_engine() {
        let c = cell();
        c.borrow_mut().as_mut().unwrap().store_mut().fail_in = Some(2);
        let mut durable = c.borrow().as_ref().unwrap().store_ref().clone();
        durable.fail_in = None;
        let r = run_in_tx(
            &c,
            |e| e.submit_batch(&actor(), "op-00000001", 0, &text("A"), 1),
            commit,
        );
        assert_eq!(
            r.unwrap_err(),
            TxFailed,
            "no `unavailable` rejection is passed through as a value"
        );
        assert!(
            c.borrow().is_none(),
            "memory is not trusted after a rollback"
        );
        // Storage rolled back (modelled by the clone taken before the call): no seq gap on rebuild.
        let mut rebuilt = Engine::open(durable, "ws-a", "site-a1").unwrap();
        assert_eq!(rebuilt.head_seq(), 0);
        assert_eq!(
            rebuilt
                .submit_batch(&actor(), "op-00000001", 0, &text("A"), 2)
                .unwrap()
                .seq,
            1
        );
    }

    #[test]
    fn a_thrown_transaction_sync_returns_err_even_when_the_flag_is_clear() {
        let c = cell();
        let durable = c.borrow().as_ref().unwrap().store_ref().clone();
        // The body ran and succeeded in memory, but the runtime's transactionSync threw
        // (so storage was rolled back): the caller must NOT see the success.
        let r = run_in_tx(
            &c,
            |e| e.submit_batch(&actor(), "op-00000001", 0, &text("A"), 1),
            |body| {
                let _ = body();
                Err(())
            },
        );
        assert_eq!(r.unwrap_err(), TxFailed);
        assert!(c.borrow().is_none());
        let rebuilt = Engine::open(durable, "ws-a", "site-a1").unwrap();
        assert_eq!(
            rebuilt.head_seq(),
            0,
            "memory and storage agree: nothing applied"
        );
        // And a runner that never even called the body is a failure too.
        let c = cell();
        let r = run_in_tx(&c, |e| e.head_seq(), |_| Err(()));
        assert_eq!(r.unwrap_err(), TxFailed);
        assert!(c.borrow().is_none());
    }

    #[test]
    fn rejections_that_are_not_storage_failures_pass_through_and_keep_the_engine() {
        let c = cell();
        let r = run_in_tx(
            &c,
            |e| e.submit_batch(&actor(), "op-00000001", 99, &text("A"), 1),
            commit,
        )
        .unwrap();
        assert!(matches!(r, Err(Rejection::InvalidBase { .. })), "{r:?}");
        assert!(c.borrow().is_some());
    }
}
