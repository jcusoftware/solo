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
use super::{CHECKPOINT_EVERY, LOG_RETAIN, MAX_UNCHECKPOINTED};

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
            seq: CHECKPOINT_EVERY
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
    let at = e.next_alarm(1_000, 0, 15_000).unwrap().unwrap();
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
    assert_eq!(e.next_alarm(1_000, 0, 15_000).unwrap(), None);
    // Connections need the membership sweep.
    assert_eq!(e.next_alarm(1_000, 2, 15_000).unwrap(), Some(16_000));
    // A presence row expiring sooner wins.
    e.presence_set("c-1", 1, "{}".into(), 1_000, 3_000).unwrap();
    assert_eq!(e.next_alarm(1_000, 2, 15_000).unwrap(), Some(4_000));
    // A checkpoint awaiting D1 wins with its backoff.
    churn(&mut e, 2, 1);
    e.take_checkpoint(1, CheckpointReason::Save, 1_000).unwrap();
    assert_eq!(e.next_alarm(1_000, 2, 15_000).unwrap(), Some(1_000 + 2_000));
    // Ops are remembered for OP_TTL: the last job to remain.
    e.mark_revision_written(2).unwrap();
    e.presence_remove("c-1").unwrap();
    let at = e.next_alarm(2_000, 0, 15_000).unwrap().unwrap();
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
