//! Engine behaviour tests over [`MemStore`]. A "hibernation" in these tests is
//! dropping the engine and reopening it from a clone of the persisted store:
//! the constructor path the Durable Object takes after eviction.

use sws_ir::edit::{Command, CommandBatch, ContainerRef, canonical_json};

use super::engine::{
    Actor, CheckpointOutcome, Engine, Rejection, SyncPlan, backoff_ms, revision_id,
};
use super::protocol::CheckpointReason;
use super::rebase::ConflictKind;
use super::store::{EntryKind, MemStore, Store};
use super::{CHECKPOINT_EVERY, DEFAULT_IDLE_SWEEP_MS, LOG_RETAIN, MAX_UNCHECKPOINTED};

const SWEEP: i64 = DEFAULT_IDLE_SWEEP_MS;

const FIXTURE: &str = include_str!("../../tests/fixtures/session-site.json");
const WS: &str = "ws-a";
const SITE: &str = "site-a1";

fn actor(user_id: i64) -> Actor {
    Actor {
        user_id,
        client_id: format!("c-{user_id}"),
    }
}

fn seeded() -> Engine<MemStore> {
    let mut e = Engine::open(MemStore::default(), WS, SITE).unwrap();
    e.seed(FIXTURE, 0).unwrap();
    e
}

fn text(uid: &str, value: &str) -> CommandBatch {
    CommandBatch::new(vec![Command::SetText {
        uid: uid.into(),
        value: value.into(),
    }])
}

fn attr(uid: &str, name: &str, value: &str) -> CommandBatch {
    CommandBatch::new(vec![Command::SetAttr {
        uid: uid.into(),
        attr: name.into(),
        value: Some(value.into()),
    }])
}

fn op(n: u32) -> String {
    format!("op-{n:08}")
}

/// Apply `n` distinct title edits as user 1, returning the next op number.
fn churn(e: &mut Engine<MemStore>, n: u32, from: u32) -> u32 {
    for i in 0..n {
        let base = e.head_seq();
        e.submit_batch(
            &actor(1),
            &op(from + i),
            base,
            &text("home.title-text", &format!("Title {}", from + i)),
            1_000 + i64::from(i),
        )
        .unwrap();
    }
    from + n
}

#[test]
fn seed_requires_an_editable_lossless_document_once() {
    let mut e = Engine::open(MemStore::default(), WS, SITE).unwrap();
    assert!(!e.seeded());
    assert_eq!(
        e.submit_batch(&actor(1), &op(1), 0, &text("a", "x"), 0)
            .unwrap_err()
            .code(),
        "not_seeded"
    );
    // Not upgraded (no stable-id marker) is refused.
    let legacy = FIXTURE.replace("\"sws.ir\"", "\"other\"");
    assert_eq!(e.seed(&legacy, 0).unwrap_err().code(), "invalid_document");
    // Unknown top-level fields would be lost on save: refused.
    let lossy = FIXTURE.replacen('{', "{\"x_ext\": 1,", 1);
    assert_eq!(e.seed(&lossy, 0).unwrap_err().code(), "invalid_document");
    assert_eq!(
        e.seed("not json", 0).unwrap_err().code(),
        "invalid_document"
    );
    e.seed(FIXTURE, 0).unwrap();
    assert!(e.seeded());
    assert_eq!(e.head_seq(), 0);
    assert_eq!(e.seed(FIXTURE, 0).unwrap_err(), Rejection::AlreadySeeded);
}

#[test]
fn log_is_monotonic_and_matches_the_reducer() {
    let mut e = seeded();
    let mut expected = e.document().unwrap().clone();
    for i in 0..5 {
        let batch = text("home.title-text", &format!("T{i}"));
        let c = e
            .submit_batch(&actor(1), &op(i), e.head_seq(), &batch, 10 + i64::from(i))
            .unwrap();
        assert_eq!(c.seq, i64::from(i) + 1);
        assert!(!c.duplicate && !c.rebased);
        let entry = c.entry.unwrap();
        assert_eq!(
            (entry.kind, entry.base_seq),
            (EntryKind::Batch, i64::from(i))
        );
        // Canonical reduction is sws_ir's: replaying the same batch yields the same document.
        expected = sws_ir::edit::apply_batch(&expected, &batch).unwrap().0;
    }
    assert_eq!(e.head_seq(), 5);
    assert_eq!(
        canonical_json(e.document().unwrap()),
        canonical_json(&expected)
    );
}

#[test]
fn invalid_batches_are_atomic_and_leave_no_trace() {
    let mut e = seeded();
    let before = canonical_json(e.document().unwrap());
    let bad = CommandBatch::new(vec![
        Command::SetText {
            uid: "home.title-text".into(),
            value: "ok".into(),
        },
        Command::RemoveNode {
            uid: "no.such.node".into(),
        },
    ]);
    let r = e.submit_batch(&actor(1), &op(1), 0, &bad, 1).unwrap_err();
    match r {
        Rejection::Command(ce) => assert_eq!(ce.command_index, Some(1)),
        other => panic!("{other:?}"),
    }
    assert_eq!(e.head_seq(), 0);
    assert_eq!(canonical_json(e.document().unwrap()), before);
    // A failed op id is not remembered: the corrected retry applies.
    e.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "ok"), 2)
        .unwrap();
    assert_eq!(e.head_seq(), 1);
}

#[test]
fn base_seq_ahead_of_head_is_rejected() {
    let mut e = seeded();
    let r = e.submit_batch(&actor(1), &op(1), 3, &text("home.title-text", "x"), 1);
    assert_eq!(r.unwrap_err().code(), "invalid_base");
}

#[test]
fn retried_op_id_is_applied_once_and_acknowledged_again() {
    let mut e = seeded();
    let batch = text("home.title-text", "Once");
    let first = e
        .submit_batch(&actor(1), "op-retry-0001", 0, &batch, 5)
        .unwrap();
    assert_eq!((first.seq, first.duplicate), (1, false));
    // Another edit lands, then the client (never saw the ack) retries the SAME op
    // with its original base: it must not be re-applied, rejected as stale, or double-logged.
    e.submit_batch(&actor(2), &op(2), 1, &attr("home.lede", "title", "t"), 6)
        .unwrap();
    let again = e
        .submit_batch(&actor(1), "op-retry-0001", 0, &batch, 7)
        .unwrap();
    assert_eq!(
        (again.seq, again.duplicate, again.entry.is_none()),
        (1, true, true)
    );
    assert_eq!(e.head_seq(), 2);
    // Undo ops are idempotent the same way.
    let u1 = e.submit_undo(&actor(1), "op-undo-0001", 1, 8).unwrap();
    let u2 = e.submit_undo(&actor(1), "op-undo-0001", 1, 9).unwrap();
    assert_eq!((u1.seq, u2.seq, u2.duplicate), (3, 3, true));
    assert_eq!(e.head_seq(), 3);
    // Someone else cannot replay my operation id.
    assert_eq!(
        e.submit_batch(&actor(9), "op-retry-0001", 0, &batch, 10)
            .unwrap_err(),
        Rejection::OpIdReused
    );
}

#[test]
fn expired_op_ids_are_forgotten() {
    let mut e = seeded();
    e.submit_batch(
        &actor(1),
        "op-expire-001",
        0,
        &text("home.title-text", "a"),
        0,
    )
    .unwrap();
    let late = super::OP_TTL_MS + 1;
    let c = e
        .submit_batch(
            &actor(1),
            "op-expire-001",
            1,
            &text("home.title-text", "b"),
            late,
        )
        .unwrap();
    assert!(!c.duplicate);
}

#[test]
fn stale_independent_field_updates_are_rebased() {
    let mut e = seeded();
    // Client A and B both start at seq 0.
    e.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "A edit"), 1)
        .unwrap();
    e.submit_batch(&actor(1), &op(2), 1, &attr("home.hero", "data-x", "1"), 2)
        .unwrap();
    // B's batch is based on 0 and touches an unrelated field and the colour token.
    let b = CommandBatch::new(vec![
        Command::SetText {
            uid: "home.menu-p-text".into(),
            value: "B soup".into(),
        },
        Command::SetColorToken {
            key: "bg".into(),
            hex: "#112233".into(),
        },
    ]);
    let c = e.submit_batch(&actor(2), &op(3), 0, &b, 3).unwrap();
    assert_eq!((c.seq, c.rebased), (3, true));
    let entry = c.entry.unwrap();
    assert_eq!(entry.base_seq, 0);
    assert!(entry.rebased);
    let doc = canonical_json(e.document().unwrap());
    assert!(doc.contains("A edit") && doc.contains("B soup") && doc.contains("#112233"));
}

#[test]
fn stale_overlapping_field_is_a_conflict_naming_the_paths() {
    let mut e = seeded();
    e.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "A"), 1)
        .unwrap();
    let r = e
        .submit_batch(&actor(2), &op(2), 0, &text("home.title-text", "B"), 2)
        .unwrap_err();
    match r {
        Rejection::Conflict(c) => {
            assert_eq!(c.kind, ConflictKind::FieldConflict);
            assert_eq!(c.seqs, vec![1]);
            assert_eq!(c.paths, vec!["node:home.title-text/text".to_string()]);
            assert_eq!(c.head_seq, 1);
            assert!(c.message.contains("since_seq 0"), "{}", c.message);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(e.head_seq(), 1, "a rejected batch changes nothing");
}

#[test]
fn stale_structural_commands_are_rejected_even_without_overlap() {
    let mut e = seeded();
    e.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "A"), 1)
        .unwrap();
    let mv = CommandBatch::new(vec![Command::MoveNode {
        uid: "home.visit".into(),
        container: ContainerRef::Node {
            uid: "home.main".into(),
            slot: None,
        },
        index: Some(0),
    }]);
    let r = e.submit_batch(&actor(2), &op(2), 0, &mv, 2).unwrap_err();
    match r {
        Rejection::Conflict(c) => {
            assert_eq!(c.kind, ConflictKind::StaleStructural);
            assert!(c.message.contains("move_node"), "{}", c.message);
        }
        other => panic!("{other:?}"),
    }
    // The same structural batch against the head is fine.
    let c = e.submit_batch(&actor(2), &op(3), 1, &mv, 3).unwrap();
    assert!(!c.rebased);
}

#[test]
fn stale_edit_of_a_removed_node_reports_target_changed() {
    let mut e = seeded();
    let rm = CommandBatch::new(vec![Command::RemoveNode {
        uid: "home.menu".into(),
    }]);
    e.submit_batch(&actor(1), &op(1), 0, &rm, 1).unwrap();
    let r = e
        .submit_batch(&actor(2), &op(2), 0, &text("home.menu-p-text", "x"), 2)
        .unwrap_err();
    match r {
        Rejection::Conflict(c) => assert_eq!(c.kind, ConflictKind::TargetChanged),
        other => panic!("{other:?}"),
    }
}

#[test]
fn undo_is_a_new_inverse_command_not_a_history_rewind() {
    let mut e = seeded();
    let original = canonical_json(e.document().unwrap());
    e.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "Changed"), 1)
        .unwrap();
    let edited = canonical_json(e.document().unwrap());
    assert_ne!(edited, original);

    let u = e.submit_undo(&actor(1), &op(2), 1, 2).unwrap();
    assert_eq!(u.seq, 2, "undo takes the next sequence number");
    let entry = u.entry.unwrap();
    assert_eq!((entry.kind, entry.target_seq), (EntryKind::Undo, Some(1)));
    assert_eq!(canonical_json(e.document().unwrap()), original);
    // Both entries are still in the log (nothing rewound).
    assert!(e.sync_plan(Some(0)).is_ok());
    match e.sync_plan(Some(0)).unwrap() {
        SyncPlan::Replay(v) => assert_eq!(v.len(), 2),
        other => panic!("{other:?}"),
    }

    // Undoing the undo is a redo, again as a new entry.
    let r = e.submit_undo(&actor(1), &op(3), 2, 3).unwrap();
    assert_eq!(r.seq, 3);
    assert_eq!(canonical_json(e.document().unwrap()), edited);
}

#[test]
fn undo_refuses_other_users_entries_and_incompatible_tails() {
    let mut e = seeded();
    e.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "A"), 1)
        .unwrap();
    assert_eq!(
        e.submit_undo(&actor(2), &op(2), 1, 2).unwrap_err(),
        Rejection::UndoNotOwner
    );
    // A later change to the same field makes user 1's undo incompatible.
    e.submit_batch(&actor(2), &op(3), 1, &text("home.title-text", "B"), 3)
        .unwrap();
    let r = e.submit_undo(&actor(1), &op(4), 1, 4).unwrap_err();
    match r {
        Rejection::Conflict(c) => {
            assert_eq!(c.kind, ConflictKind::UndoIncompatible);
            assert_eq!(c.seqs, vec![2]);
            assert_eq!(c.paths, vec!["node:home.title-text/text".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    // An unrelated later change does not block undo.
    e.submit_batch(&actor(2), &op(5), 2, &attr("home.lede", "title", "t"), 5)
        .unwrap();
    e.submit_batch(&actor(1), &op(6), 3, &attr("home.hero", "data-y", "1"), 6)
        .unwrap();
    assert!(e.submit_undo(&actor(1), &op(7), 4, 7).is_ok());
    assert_eq!(
        e.submit_undo(&actor(1), &op(8), 99, 8).unwrap_err().code(),
        "not_found"
    );
}

#[test]
fn undo_after_a_structural_change_to_the_same_node_is_refused() {
    let mut e = seeded();
    e.submit_batch(&actor(1), &op(1), 0, &text("home.menu-p-text", "x"), 1)
        .unwrap();
    let rm = CommandBatch::new(vec![Command::RemoveNode {
        uid: "home.menu".into(),
    }]);
    e.submit_batch(&actor(2), &op(2), 1, &rm, 2).unwrap();
    let r = e.submit_undo(&actor(1), &op(3), 1, 3).unwrap_err();
    assert!(matches!(r, Rejection::Conflict(c) if c.kind == ConflictKind::UndoIncompatible));
}

#[test]
fn checkpoint_every_n_commands_queues_exactly_one_revision() {
    let mut e = seeded();
    let mut n = 1;
    n = churn(&mut e, (CHECKPOINT_EVERY - 1) as u32, n);
    assert!(!e.checkpoint_due(), "49 commands: not due");
    churn(&mut e, 1, n);
    assert_eq!(e.head_seq(), CHECKPOINT_EVERY);
    assert!(e.checkpoint_due());

    let out = e.take_checkpoint(1, CheckpointReason::Auto, 9_000).unwrap();
    let CheckpointOutcome::Taken(p) = out else {
        panic!("{out:?}")
    };
    assert_eq!((p.seq, p.created_by, p.attempts), (CHECKPOINT_EVERY, 1, 0));
    assert_eq!(p.reason, "auto");
    assert_eq!(p.ir_sha256.len(), 64);
    assert_eq!(
        p.revision_id,
        revision_id(WS, SITE, CHECKPOINT_EVERY, &p.ir_sha256)
    );
    // Not due again, and a second take reports the outstanding one instead of a new row.
    assert!(!e.checkpoint_due());
    assert!(matches!(
        e.take_checkpoint(1, CheckpointReason::Save, 9_001).unwrap(),
        CheckpointOutcome::Pending(q) if q == p
    ));
    // Confirming the D1 write clears it; with no new commands a save is a no-op.
    assert!(e.mark_revision_written(p.seq).unwrap());
    assert!(!e.mark_revision_written(p.seq).unwrap(), "idempotent");
    assert_eq!(
        e.take_checkpoint(1, CheckpointReason::Save, 9_002).unwrap(),
        CheckpointOutcome::Unchanged {
            seq: CHECKPOINT_EVERY,
            revision_id: p.revision_id.clone(),
        }
    );
    assert!(!e.checkpoint_due());
    // 49 more commands: still not due; the 50th makes it due again.
    let n2 = churn(&mut e, 49, 100);
    assert!(!e.checkpoint_due());
    churn(&mut e, 1, n2);
    assert!(e.checkpoint_due());
}

#[test]
fn explicit_save_checkpoints_before_the_threshold_and_seed_is_checkpointable() {
    let mut e = seeded();
    // The seed itself can be saved once.
    let CheckpointOutcome::Taken(p0) = e.take_checkpoint(1, CheckpointReason::Save, 1).unwrap()
    else {
        panic!()
    };
    assert_eq!(p0.seq, 0);
    e.mark_revision_written(0).unwrap();
    churn(&mut e, 3, 1);
    let CheckpointOutcome::Taken(p) = e.take_checkpoint(2, CheckpointReason::Publish, 2).unwrap()
    else {
        panic!()
    };
    assert_eq!((p.seq, p.created_by, p.reason.as_str()), (3, 2, "publish"));
    assert_ne!(p.revision_id, p0.revision_id);
}

#[test]
fn checkpoint_retry_state_survives_hibernation() {
    let mut e = seeded();
    churn(&mut e, 5, 1);
    let CheckpointOutcome::Taken(p) = e.take_checkpoint(1, CheckpointReason::Save, 1).unwrap()
    else {
        panic!()
    };
    e.note_revision_failure().unwrap();
    e.note_revision_failure().unwrap();
    // Evicted before D1 confirmed: the pending row and attempt count come back.
    let mut e = Engine::open(e.into_store(), WS, SITE).unwrap();
    let q = e.pending().cloned().unwrap();
    assert_eq!(
        (q.revision_id.as_str(), q.attempts),
        (p.revision_id.as_str(), 2)
    );
    // Backoff grows and is capped; the next alarm honours it.
    assert_eq!(backoff_ms(0), 2_000);
    assert_eq!(backoff_ms(2), 8_000);
    assert_eq!(backoff_ms(40), 256_000);
    let at = e.next_alarm(1_000, 0, SWEEP).unwrap().unwrap();
    assert_eq!(at, 1_000 + 8_000);
    e.mark_revision_written(q.seq).unwrap();
    assert!(e.pending().is_none());
}

#[test]
fn unresolved_checkpoint_bounds_the_backlog() {
    let mut e = seeded();
    churn(&mut e, 1, 1);
    e.take_checkpoint(1, CheckpointReason::Save, 1).unwrap();
    // D1 never confirms: edits are accepted up to the bound, then refused.
    let mut n = 10;
    for _ in 0..MAX_UNCHECKPOINTED {
        n = churn(&mut e, 1, n);
    }
    let r = e
        .submit_batch(
            &actor(1),
            &op(9_999),
            e.head_seq(),
            &text("home.title-text", "x"),
            1,
        )
        .unwrap_err();
    assert_eq!(r, Rejection::CheckpointBacklog);
    e.mark_revision_written(1).unwrap();
    assert!(
        e.submit_batch(
            &actor(1),
            &op(9_998),
            e.head_seq(),
            &text("home.title-text", "y"),
            2
        )
        .is_ok()
    );
}

#[test]
fn hibernation_restores_the_document_log_and_idempotency() {
    let mut e = seeded();
    churn(&mut e, 7, 1);
    let live = canonical_json(e.document().unwrap());
    let head = e.head_seq();

    // Evicted with only the log (no checkpoint yet): snapshot 0 + replay.
    let mut woke = Engine::open(e.into_store(), WS, SITE).unwrap();
    assert_eq!(woke.head_seq(), head);
    assert_eq!(canonical_json(woke.document().unwrap()), live);
    // The retry of an op applied before eviction is still a duplicate.
    let dup = woke
        .submit_batch(
            &actor(1),
            &op(3),
            2,
            &text("home.title-text", "Title 3"),
            50,
        )
        .unwrap();
    assert!(dup.duplicate);

    // Checkpoint, more commands, evict again: newest snapshot + short replay.
    woke.take_checkpoint(1, CheckpointReason::Save, 60).unwrap();
    woke.mark_revision_written(head).unwrap();
    churn(&mut woke, 3, 100);
    let live2 = canonical_json(woke.document().unwrap());
    let woke2 = Engine::open(woke.into_store(), WS, SITE).unwrap();
    assert_eq!(woke2.head_seq(), head + 3);
    assert_eq!(canonical_json(woke2.document().unwrap()), live2);
    assert_eq!(woke2.checkpoint_seq(), Some(head));
}

#[test]
fn hibernation_replays_undo_entries_too() {
    let mut e = seeded();
    e.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "A"), 1)
        .unwrap();
    e.submit_undo(&actor(1), &op(2), 1, 2).unwrap();
    e.submit_batch(&actor(1), &op(3), 2, &text("home.title-text", "C"), 3)
        .unwrap();
    let live = canonical_json(e.document().unwrap());
    let woke = Engine::open(e.into_store(), WS, SITE).unwrap();
    assert_eq!(canonical_json(woke.document().unwrap()), live);
    assert_eq!(woke.head_seq(), 3);
}

#[test]
fn opening_with_another_identity_is_refused() {
    let e = seeded();
    let r = Engine::open(e.into_store(), "ws-b", SITE);
    assert!(matches!(
        r,
        Err(super::engine::EngineError::IdentityMismatch)
    ));
}

#[test]
fn corrupt_logs_fail_closed() {
    let mut e = seeded();
    churn(&mut e, 3, 1);
    let mut store = e.into_store();
    store.log.remove(&2);
    assert!(matches!(
        Engine::open(store, WS, SITE),
        Err(super::engine::EngineError::Corrupt(_))
    ));
}

#[test]
fn compaction_trims_the_log_but_keeps_idempotency_and_recent_rebase() {
    let mut e = seeded();
    churn(&mut e, 80, 1);
    assert!(!e.needs_compaction(), "no snapshot past the seed yet");
    e.take_checkpoint(1, CheckpointReason::Save, 5).unwrap();
    e.mark_revision_written(80).unwrap();
    assert!(e.needs_compaction());
    let h = e.compact(6).unwrap();
    assert_eq!(h, Some(80 - LOG_RETAIN));
    // Retry-safe: running it again (alarm re-delivery) is a no-op.
    assert_eq!(e.compact(7).unwrap(), None);
    assert!(!e.needs_compaction());
    assert_eq!(e.meta().compacted_through, 30);

    // An old op id is still deduplicated after its log entry is gone.
    let dup = e
        .submit_batch(&actor(1), &op(2), 1, &text("home.title-text", "Title 2"), 8)
        .unwrap();
    assert!(dup.duplicate);
    // A batch based before the horizon must resync; one inside the window rebases.
    let too_old = e
        .submit_batch(
            &actor(2),
            &op(500),
            10,
            &attr("home.hero", "data-z", "1"),
            9,
        )
        .unwrap_err();
    assert!(matches!(&too_old, Rejection::Conflict(c) if c.kind == ConflictKind::BaseTooOld));
    let ok = e
        .submit_batch(
            &actor(2),
            &op(501),
            60,
            &attr("home.hero", "data-z", "1"),
            10,
        )
        .unwrap();
    assert!(ok.rebased);
    // Undo of a compacted entry is refused with a clear conflict.
    let u = e.submit_undo(&actor(1), &op(502), 5, 11).unwrap_err();
    assert!(matches!(&u, Rejection::Conflict(c) if c.kind == ConflictKind::UndoTooOld));
    // Sync falls back to the full state when the gap predates the log.
    assert_eq!(e.sync_plan(Some(5)).unwrap(), SyncPlan::Full);
    assert!(matches!(
        e.sync_plan(Some(70)).unwrap(),
        SyncPlan::Replay(_)
    ));

    // The compacted store still wakes up correctly.
    let live = canonical_json(e.document().unwrap());
    let woke = Engine::open(e.into_store(), WS, SITE).unwrap();
    assert_eq!(canonical_json(woke.document().unwrap()), live);
}

#[test]
fn compaction_never_touches_entries_after_the_snapshot() {
    let mut e = seeded();
    churn(&mut e, 60, 1);
    e.take_checkpoint(1, CheckpointReason::Save, 1).unwrap();
    churn(&mut e, 10, 200);
    e.compact(2).unwrap();
    let store = e.into_store();
    let snap = store.latest_snapshot().unwrap().unwrap().seq;
    assert_eq!(snap, 60);
    assert!(store.log.keys().all(|s| *s > 60 - LOG_RETAIN));
    assert!(store.log.contains_key(&61) && store.log.contains_key(&70));
}

#[test]
fn sync_plans() {
    let mut e = seeded();
    churn(&mut e, 4, 1);
    assert_eq!(e.sync_plan(None).unwrap(), SyncPlan::Full);
    assert_eq!(e.sync_plan(Some(99)).unwrap(), SyncPlan::Full);
    match e.sync_plan(Some(4)).unwrap() {
        SyncPlan::Replay(v) => assert!(v.is_empty()),
        other => panic!("{other:?}"),
    }
    match e.sync_plan(Some(1)).unwrap() {
        SyncPlan::Replay(v) => {
            assert_eq!(v.iter().map(|x| x.seq).collect::<Vec<_>>(), vec![2, 3, 4])
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn presence_expires_and_is_rate_limited() {
    let mut e = seeded();
    let ttl = 1_000;
    e.presence_set("c-1", 1, r#"{"cursor":"a"}"#.into(), 0, ttl)
        .unwrap();
    e.presence_set("c-2", 2, r#"{"cursor":"b"}"#.into(), 10, ttl)
        .unwrap();
    assert_eq!(e.presence_list(500).unwrap().len(), 2);
    // Too-frequent updates from one client are refused.
    assert_eq!(
        e.presence_set("c-1", 1, "{}".into(), 20, ttl).unwrap_err(),
        Rejection::RateLimited
    );
    // c-1 refreshes and outlives c-2... after ttl only the refreshed one remains.
    e.presence_set("c-1", 1, r#"{"cursor":"c"}"#.into(), 600, ttl)
        .unwrap();
    let live = e.presence_list(1_100).unwrap();
    assert_eq!(
        live.iter()
            .map(|p| p.client_id.as_str())
            .collect::<Vec<_>>(),
        vec!["c-1"]
    );
    assert_eq!(e.presence_prune(1_100).unwrap(), 1);
    assert_eq!(e.presence_list(1_700).unwrap().len(), 0);
    e.presence_set("c-3", 3, "{}".into(), 2_000, ttl).unwrap();
    e.presence_remove("c-3").unwrap();
    assert!(e.presence_list(2_001).unwrap().is_empty());
}

#[test]
fn presence_survives_hibernation_until_it_expires() {
    let mut e = seeded();
    e.presence_set("c-1", 1, r#"{"k":1}"#.into(), 0, 5_000)
        .unwrap();
    let woke = Engine::open(e.into_store(), WS, SITE).unwrap();
    assert_eq!(woke.presence_list(100).unwrap().len(), 1);
    assert_eq!(woke.presence_list(5_001).unwrap().len(), 0);
}

#[test]
fn alarm_planning_covers_every_outstanding_job() {
    let mut e = seeded();
    // Nothing outstanding, nobody connected: no alarm.
    assert_eq!(e.next_alarm(1_000, 0, SWEEP).unwrap(), None);
    // Connections need the membership sweep.
    assert_eq!(e.next_alarm(1_000, 2, SWEEP).unwrap(), Some(1_000 + SWEEP));
    // A presence row expiring sooner wins.
    e.presence_set("c-1", 1, "{}".into(), 1_000, 3_000).unwrap();
    assert_eq!(e.next_alarm(1_000, 2, SWEEP).unwrap(), Some(4_000));
    // A checkpoint awaiting D1 wins with its backoff.
    churn(&mut e, 2, 1);
    e.take_checkpoint(1, CheckpointReason::Save, 1_000).unwrap();
    assert_eq!(e.next_alarm(1_000, 2, SWEEP).unwrap(), Some(1_000 + 2_000));
    // Ops are remembered for OP_TTL: the last job to remain.
    e.mark_revision_written(2).unwrap();
    e.presence_remove("c-1").unwrap();
    let at = e.next_alarm(2_000, 0, SWEEP).unwrap().unwrap();
    assert!(at > 2_000 + 60_000, "{at}");
}

#[test]
fn revision_ids_are_deterministic_and_scoped() {
    let a = revision_id(WS, SITE, 50, &"a".repeat(64));
    assert_eq!(a, revision_id(WS, SITE, 50, &"a".repeat(64)));
    assert_ne!(a, revision_id("ws-b", SITE, 50, &"a".repeat(64)));
    assert_ne!(a, revision_id(WS, SITE, 51, &"a".repeat(64)));
    assert_eq!(a.len(), 27);
    assert!(a.starts_with("ck-") && a[3..].bytes().all(|b| b.is_ascii_hexdigit()));
}

// --- Review fixes ----------------------------------------------------------------

use super::engine::{FailurePlan, is_permanent_d1_error, plan_checkpoint_failure};

#[test]
fn a_reused_op_id_with_a_different_request_is_refused_not_acked() {
    let mut e = seeded();
    let first = e
        .submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "A"), 1)
        .unwrap();
    // Same user, same op id, SAME payload: the idempotent retry.
    assert!(
        e.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "A"), 2)
            .unwrap()
            .duplicate
    );
    // Same user, same op id, different content: never a silent duplicate ack.
    let r = e
        .submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "B"), 3)
        .unwrap_err();
    assert_eq!(r, Rejection::OpIdReused);
    assert_eq!(r.code(), "op_id_reused");
    // Same op id reused as a different kind (undo) is refused too.
    assert_eq!(
        e.submit_undo(&actor(1), &op(1), first.seq, 4).unwrap_err(),
        Rejection::OpIdReused
    );
    // An undo retried with its own payload is a duplicate; with another target it is not.
    let u = e.submit_undo(&actor(1), &op(2), 1, 5).unwrap();
    assert!(e.submit_undo(&actor(1), &op(2), 1, 6).unwrap().duplicate);
    assert_eq!(
        e.submit_undo(&actor(1), &op(2), 2, 7).unwrap_err(),
        Rejection::OpIdReused
    );
    assert_eq!(e.head_seq(), u.seq, "none of the refused requests applied");
    // The fingerprint is durable: it survives hibernation.
    let mut woke = Engine::open(e.into_store(), WS, SITE).unwrap();
    assert_eq!(
        woke.submit_batch(&actor(1), &op(1), 0, &text("home.title-text", "B"), 8)
            .unwrap_err(),
        Rejection::OpIdReused
    );
}

#[test]
fn unchanged_save_and_publish_name_the_last_checkpoint() {
    let mut e = seeded();
    churn(&mut e, 3, 1);
    let CheckpointOutcome::Taken(p) = e.take_checkpoint(1, CheckpointReason::Save, 1).unwrap()
    else {
        panic!()
    };
    e.mark_revision_written(p.seq).unwrap();
    // Publishing a site with nothing new publishes THAT checkpoint (never an empty id)...
    let out = e.take_checkpoint(1, CheckpointReason::Publish, 2).unwrap();
    assert_eq!(
        out,
        CheckpointOutcome::Unchanged {
            seq: 3,
            revision_id: p.revision_id.clone()
        }
    );
    // ...and its bytes are kept as the publish snapshot even after later checkpoints.
    churn(&mut e, 2, 10);
    let CheckpointOutcome::Taken(q) = e.take_checkpoint(1, CheckpointReason::Save, 3).unwrap()
    else {
        panic!()
    };
    e.mark_revision_written(q.seq).unwrap();
    let kept = e.revision_snapshot(&p.revision_id).unwrap().unwrap();
    assert_eq!(kept.seq, 3);
    assert_eq!(kept.sha256, p.ir_sha256);
}

#[test]
fn checkpoint_snapshots_are_retained_for_the_latest_publish_and_latest_checkpoint_only() {
    let mut e = seeded();
    let mut n = churn(&mut e, 2, 1);
    let CheckpointOutcome::Taken(save1) = e.take_checkpoint(1, CheckpointReason::Save, 1).unwrap()
    else {
        panic!()
    };
    e.mark_revision_written(save1.seq).unwrap();
    n = churn(&mut e, 2, n);
    let CheckpointOutcome::Taken(publish1) =
        e.take_checkpoint(1, CheckpointReason::Publish, 2).unwrap()
    else {
        panic!()
    };
    // Pending checkpoints are fetchable too (the id is already announced).
    assert!(
        e.revision_snapshot(&publish1.revision_id)
            .unwrap()
            .is_some()
    );
    assert!(
        e.revision_snapshot(&save1.revision_id).unwrap().is_some(),
        "the latest CONFIRMED checkpoint stays fetchable while a newer one is pending"
    );
    e.mark_revision_written(publish1.seq).unwrap();
    // Later auto/save checkpoints do not evict the latest publish snapshot.
    for i in 0..3 {
        n = churn(&mut e, 2, n);
        let CheckpointOutcome::Taken(p) = e
            .take_checkpoint(1, CheckpointReason::Save, 10 + i)
            .unwrap()
        else {
            panic!()
        };
        e.mark_revision_written(p.seq).unwrap();
    }
    let kept = e.revision_snapshot(&publish1.revision_id).unwrap().unwrap();
    assert_eq!((kept.seq, kept.sha256), (4, publish1.ir_sha256.clone()));
    let store = e.into_store();
    assert_eq!(
        store.snapshots.keys().copied().collect::<Vec<_>>(),
        vec![4, 8, 10],
        "bounded: the latest publish, the previous confirmed checkpoint (pruned at the next one) and the latest"
    );
    assert!(store.snapshots.len() <= 3);
    // After a restart the same lookups work (the refs live in meta).
    let woke = Engine::open(store, WS, SITE).unwrap();
    assert!(
        woke.revision_snapshot(&publish1.revision_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        woke.revision_snapshot("ck-000000000000000000000000")
            .unwrap(),
        None
    );
    // A newer publish replaces the retained one.
    let mut woke = woke;
    churn(&mut woke, 1, 99);
    let CheckpointOutcome::Taken(publish2) = woke
        .take_checkpoint(1, CheckpointReason::Publish, 50)
        .unwrap()
    else {
        panic!()
    };
    woke.mark_revision_written(publish2.seq).unwrap();
    churn(&mut woke, 1, 120);
    woke.take_checkpoint(1, CheckpointReason::Save, 51).unwrap();
    woke.mark_revision_written(publish2.seq + 1).unwrap();
    assert!(
        woke.revision_snapshot(&publish1.revision_id)
            .unwrap()
            .is_none()
    );
    assert!(
        woke.revision_snapshot(&publish2.revision_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn the_last_confirmed_revision_stays_fetchable_while_a_newer_checkpoint_is_pending() {
    // A D1 outage: checkpoint B is taken but never confirmed. Revision A (confirmed
    // earlier) and the confirmed publish P must stay fetchable; at most three rows.
    let mut e = seeded();
    let mut n = churn(&mut e, 2, 1);
    let CheckpointOutcome::Taken(p) = e.take_checkpoint(1, CheckpointReason::Publish, 1).unwrap()
    else {
        panic!()
    };
    e.mark_revision_written(p.seq).unwrap();
    n = churn(&mut e, 2, n);
    let CheckpointOutcome::Taken(a) = e.take_checkpoint(1, CheckpointReason::Save, 2).unwrap()
    else {
        panic!()
    };
    e.mark_revision_written(a.seq).unwrap();
    n = churn(&mut e, 2, n);
    let CheckpointOutcome::Taken(b) = e.take_checkpoint(1, CheckpointReason::Auto, 3).unwrap()
    else {
        panic!()
    };
    // B is pending (D1 down); more edits and failed retries do not drop A or P.
    e.note_revision_failure().unwrap();
    churn(&mut e, 3, n);
    for r in [&p, &a, &b] {
        let got = e.revision_snapshot(&r.revision_id).unwrap();
        assert_eq!(got.map(|s| s.seq), Some(r.seq), "{}", r.revision_id);
    }
    assert_eq!(e.meta().last_revision.as_ref().unwrap().seq, a.seq);
    let store = e.into_store();
    assert_eq!(
        store.snapshots.keys().copied().collect::<Vec<_>>(),
        vec![p.seq, a.seq, b.seq],
        "pending + latest confirmed + latest confirmed publish, no more"
    );
    // The same after the object is evicted and rebuilt.
    let woke = Engine::open(store, WS, SITE).unwrap();
    for r in [&p, &a, &b] {
        assert!(woke.revision_snapshot(&r.revision_id).unwrap().is_some());
    }
}

#[test]
fn a_storage_failure_mid_append_poisons_the_engine_and_a_rollback_is_consistent() {
    let mut e = seeded();
    churn(&mut e, 2, 1);
    // What the object does for a failed call: the DO transaction rolls storage
    // back to its state before the call...
    let before_call = e.into_store();
    let mut store = before_call.clone();
    // ...here modelled by restoring `before_call`. The append is a two-statement
    // turn (log row, then op id): fail the second.
    store.fail_in = Some(2);
    let mut e = Engine::open(store, WS, SITE).unwrap();
    assert!(!e.store_failed());
    let r = e
        .submit_batch(&actor(1), &op(50), 2, &text("home.title-text", "X"), 5)
        .unwrap_err();
    assert_eq!(r.code(), "unavailable", "the client may retry");
    assert!(e.store_failed(), "the object must drop this engine");
    assert!(!e.store_failed(), "reported once");
    assert_eq!(e.head_seq(), 2, "memory did not advance past storage");
    // The partial write (log row without its op id) is exactly what a rollback removes.
    let half_written = e.into_store();
    assert!(half_written.log.contains_key(&3) && !half_written.ops.contains_key(&op(50)));
    // Rolled back and rebuilt from storage: the retry with the same op id applies ONCE.
    let mut rebuilt = Engine::open(before_call, WS, SITE).unwrap();
    assert_eq!(rebuilt.head_seq(), 2);
    let ok = rebuilt
        .submit_batch(&actor(1), &op(50), 2, &text("home.title-text", "X"), 6)
        .unwrap();
    assert_eq!((ok.seq, ok.duplicate), (3, false));
    assert!(
        rebuilt
            .submit_batch(&actor(1), &op(50), 2, &text("home.title-text", "X"), 7)
            .unwrap()
            .duplicate
    );
    assert_eq!(rebuilt.head_seq(), 3);
}

#[test]
fn a_failed_checkpoint_or_compaction_never_leaves_memory_ahead_of_storage() {
    // take_checkpoint: snapshot, prune, then meta; fail each in turn.
    for fail_at in 1..=3 {
        let mut e = seeded();
        churn(&mut e, 4, 1);
        let mut store = e.into_store();
        let durable = store.clone();
        store.fail_in = Some(fail_at);
        let mut e = Engine::open(store, WS, SITE).unwrap();
        let meta_before = e.meta().clone();
        let r = e.take_checkpoint(1, CheckpointReason::Save, 9).unwrap_err();
        assert_eq!(r.code(), "unavailable", "fail_at {fail_at}");
        assert!(e.store_failed());
        assert_eq!(e.meta(), &meta_before, "meta unchanged (fail_at {fail_at})");
        assert!(e.pending().is_none() && !e.checkpoint_due());
        // After the rollback the same checkpoint can be taken cleanly.
        let mut again = Engine::open(durable, WS, SITE).unwrap();
        assert!(matches!(
            again
                .take_checkpoint(1, CheckpointReason::Save, 10)
                .unwrap(),
            CheckpointOutcome::Taken(_)
        ));
    }
    // compact: meta is only adopted after save_meta succeeded.
    let mut e = seeded();
    churn(&mut e, 80, 1);
    e.take_checkpoint(1, CheckpointReason::Save, 5).unwrap();
    e.mark_revision_written(80).unwrap();
    let mut store = e.into_store();
    store.fail_in = Some(3); // prune_ops, compact_log, then save_meta
    let mut e = Engine::open(store, WS, SITE).unwrap();
    assert!(e.compact(6).is_err());
    assert!(e.store_failed());
    assert_eq!(e.meta().compacted_through, 0, "memory did not move ahead");
    // The failed compaction is retried and then succeeds (idempotent).
    assert_eq!(e.compact(7).unwrap(), Some(80 - LOG_RETAIN));
}

#[test]
fn seed_failure_leaves_the_site_unseeded_in_memory() {
    let store = MemStore {
        fail_in: Some(2), // snapshot ok, meta fails
        ..MemStore::default()
    };
    let mut e = Engine::open(store, WS, SITE).unwrap();
    assert!(e.seed(FIXTURE, 0).is_err());
    assert!(e.store_failed());
    assert!(!e.seeded() && e.meta().snapshot_seq == 0);
}

#[test]
fn constraint_errors_are_permanent_and_outages_are_not() {
    assert!(is_permanent_d1_error(
        "D1_ERROR: FOREIGN KEY constraint failed: SQLITE_CONSTRAINT"
    ));
    assert!(is_permanent_d1_error("D1_ERROR: CHECK constraint failed"));
    assert!(is_permanent_d1_error(
        "D1_ERROR: UNIQUE constraint failed: site_revisions.id: SQLITE_CONSTRAINT_UNIQUE"
    ));
    assert!(is_permanent_d1_error(
        "D1_ERROR: abort: SQLITE_CONSTRAINT_TRIGGER"
    ));
    // The word alone is not enough: only SQLite's constraint verdicts are permanent.
    assert!(!is_permanent_d1_error("constraint solver timed out"));
    assert!(!is_permanent_d1_error(
        "Too many constraints on the request"
    ));
    assert!(!is_permanent_d1_error("D1_ERROR: Network connection lost"));
    assert!(!is_permanent_d1_error("simulated D1 outage"));
    let fk = "FOREIGN KEY constraint failed";
    assert_eq!(
        plan_checkpoint_failure("timeout", 5, Some(Some(1))),
        FailurePlan::Retry
    );
    assert_eq!(
        plan_checkpoint_failure(fk, 5, Some(Some(1))),
        FailurePlan::Reattribute(1)
    );
    // Already the owner, or no owner: nothing to re-attribute to.
    assert_eq!(
        plan_checkpoint_failure(fk, 1, Some(Some(1))),
        FailurePlan::Abandon
    );
    assert_eq!(
        plan_checkpoint_failure(fk, 5, Some(None)),
        FailurePlan::Abandon
    );
    // The owner lookup itself failing is an outage, not a verdict.
    assert_eq!(plan_checkpoint_failure(fk, 5, None), FailurePlan::Retry);
}

#[test]
fn a_permanently_failing_checkpoint_is_reattributed_or_dropped_so_edits_resume() {
    // Wedged: a pending checkpoint that never confirms blocks edits at the backlog bound.
    let mut e = seeded();
    churn(&mut e, 1, 1);
    let CheckpointOutcome::Taken(p) = e.take_checkpoint(7, CheckpointReason::Save, 1).unwrap()
    else {
        panic!()
    };
    let mut n = 10;
    for _ in 0..MAX_UNCHECKPOINTED {
        n = churn(&mut e, 1, n);
    }
    let blocked = |e: &mut Engine<MemStore>, k: u32| {
        e.submit_batch(
            &actor(1),
            &op(k),
            e.head_seq(),
            &text("home.title-text", "x"),
            1,
        )
    };
    assert_eq!(
        blocked(&mut e, 9_000).unwrap_err(),
        Rejection::CheckpointBacklog
    );

    // Re-attribution keeps the checkpoint and resets its backoff.
    e.note_revision_failure().unwrap();
    e.reattribute_pending(1).unwrap();
    let q = e.pending().unwrap();
    assert_eq!((q.created_by, q.attempts, q.seq), (1, 0, p.seq));
    assert_eq!(
        q.revision_id, p.revision_id,
        "same row id: retries stay idempotent"
    );
    assert_eq!(
        blocked(&mut e, 9_001).unwrap_err(),
        Rejection::CheckpointBacklog
    );

    // Abandoning drops it, restores the bookkeeping and edits resume.
    let dropped = e.abandon_pending().unwrap().unwrap();
    assert_eq!(dropped.revision_id, p.revision_id);
    assert!(e.pending().is_none());
    assert_eq!(
        e.meta().last_checkpoint_seq,
        None,
        "back to before the dropped checkpoint"
    );
    assert!(blocked(&mut e, 9_002).is_ok(), "edits resume");
    assert_eq!(e.abandon_pending().unwrap(), None, "idempotent");
    assert_eq!(e.meta().abandoned_seq, Some(p.seq));
    assert!(
        e.checkpoint_due(),
        "head is far past the dropped checkpoint: retry now"
    );
    // An explicit save can always take a fresh checkpoint.
    assert!(matches!(
        e.take_checkpoint(1, CheckpointReason::Save, 3).unwrap(),
        CheckpointOutcome::Taken(_)
    ));
    // Survives hibernation.
    let woke = Engine::open(e.into_store(), WS, SITE).unwrap();
    assert!(woke.pending().is_some(), "the fresh save is pending");
}

#[test]
fn engine_level_checkpoint_backlog_names_the_cause() {
    // (The Durable Object test drives this over a real socket with D1 unavailable.)
    let r = Rejection::CheckpointBacklog;
    assert_eq!(r.code(), "checkpoint_backlog");
    assert!(r.message().contains(&MAX_UNCHECKPOINTED.to_string()));
}

#[test]
fn delivery_stops_within_the_gate_when_d1_stays_unavailable() {
    use super::authz::{delivery_allowed, needs_recheck};
    use super::{DEFAULT_AUTHZ_RECHECK_MS as R, MAX_AUTHZ_RECHECK_MS as MAX};
    // A socket verified at t=0; D1 then goes away so no recheck can succeed. Walk time:
    // it is due for a recheck from R, still delivered to until 2R, never after.
    let checked = 0;
    assert!(!needs_recheck(R - 1, checked, R) && needs_recheck(R, checked, R));
    assert!(delivery_allowed(2 * R - 1, checked, R));
    assert!(!delivery_allowed(2 * R, checked, R));
    // Even at the configured maximum the delivery window is 60 s, whatever the
    // idle sweep cadence is (the sweep only closes idle sockets).
    assert!(!delivery_allowed(2 * MAX, checked, MAX));
    assert_eq!(2 * MAX, 60_000);
}

#[test]
fn a_persistent_failure_is_retried_every_n_commands_not_on_every_edit() {
    let mut e = seeded();
    churn(&mut e, CHECKPOINT_EVERY as u32, 1);
    assert!(e.checkpoint_due());
    e.take_checkpoint(1, CheckpointReason::Auto, 1).unwrap();
    e.abandon_pending().unwrap();
    assert!(
        !e.checkpoint_due(),
        "just dropped: do not hammer D1 on the next edit"
    );
    churn(&mut e, (CHECKPOINT_EVERY - 1) as u32, 1_000);
    assert!(!e.checkpoint_due());
    churn(&mut e, 1, 2_000);
    assert!(
        e.checkpoint_due(),
        "after another N commands it tries again"
    );
}
