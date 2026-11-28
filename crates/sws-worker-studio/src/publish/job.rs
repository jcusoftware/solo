//! The publish job state machine (pure, host-tested).
//!
//! ```text
//! queued ──cli──▶ validating ──▶ uploading ──▶ ready
//!    └──server──▶ compiling ──▶ validating        (compiling is not implemented)
//! any non-terminal phase ──▶ failed
//! ```
//!
//! Concurrency model. A driver (the Cron Trigger, an optional Queue consumer, or a
//! test hook) first CLAIMS a job: one conditional `UPDATE … WHERE phase = ? AND
//! attempt = ?` that bumps `attempt` and sets a lease. The new `attempt` is the
//! driver's ownership token; every later transition is conditional on
//! `(phase, attempt)` too, so a duplicate delivery (the same pointer twice), a second
//! driver racing the first, or a driver that outlived its lease changes zero rows and
//! stops. [`guard_matches`] is the exact predicate those `WHERE` clauses implement.
//!
//! Failure model. A step either advances, fails permanently (a phase-specific error
//! code), or fails transiently: the job stays in its phase with `next_attempt_at`
//! pushed out by [`backoff_secs`], until [`MAX_ATTEMPTS`] claims have been spent and
//! it fails with [`RETRIES_EXHAUSTED`]. A driver that crashes mid-job leaves the lease
//! behind; once it expires the next run reclaims the job from the phase it reached.

/// Claims a job may consume (each claim is one attempt, crashed or not).
pub const MAX_ATTEMPTS: u32 = 5;
/// How long a claim owns the job. A driver finishes in seconds; a crashed driver's
/// job is recoverable this long after its last transition.
pub const LEASE_SECS: u64 = 120;
/// First retry delay; doubles per attempt.
pub const BACKOFF_BASE_SECS: u64 = 30;
/// Upper bound on any single retry delay.
pub const BACKOFF_MAX_SECS: u64 = 3600;
/// Error code of a job whose transient failures (or crashes) used every attempt.
pub const RETRIES_EXHAUSTED: &str = "retries_exhausted";

macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &[$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            /// Exact, case-sensitive parse; unknown input is `None`.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$variant),)+ _ => None }
            }
        }
    };
}

str_enum!(
    /// `publish_jobs.phase` (CHECK in `migrations/0004_publish_jobs.sql`).
    Phase {
        Queued => "queued",
        Validating => "validating",
        Compiling => "compiling",
        Uploading => "uploading",
        Ready => "ready",
        Failed => "failed",
    }
);

str_enum!(
    /// Who produced the artifact. `Cli` artifacts are compiled by the publisher's
    /// own machine (#68) and are NEVER labelled as server-verified.
    Origin { Cli => "cli", Server => "server" }
);

impl Phase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Phase::Ready | Phase::Failed)
    }
}

/// The single forward edge out of `from`; `None` for terminal phases.
pub fn next_phase(from: Phase, origin: Origin) -> Option<Phase> {
    match (from, origin) {
        (Phase::Queued, Origin::Cli) => Some(Phase::Validating),
        (Phase::Queued, Origin::Server) => Some(Phase::Compiling),
        (Phase::Compiling, Origin::Server) => Some(Phase::Validating),
        (Phase::Compiling, Origin::Cli) => None,
        (Phase::Validating, _) => Some(Phase::Uploading),
        (Phase::Uploading, _) => Some(Phase::Ready),
        (Phase::Ready | Phase::Failed, _) => None,
    }
}

/// Every legal edge: the forward edge, or any non-terminal phase to `Failed`.
/// Staying in a phase (a transient retry) is not a transition.
pub fn can_transition(from: Phase, to: Phase, origin: Origin) -> bool {
    next_phase(from, origin) == Some(to) || (to == Phase::Failed && !from.is_terminal())
}

/// Retry delay after the `attempt`-th claim failed transiently:
/// `BACKOFF_BASE_SECS * 2^(attempt-1)`, capped at [`BACKOFF_MAX_SECS`].
pub fn backoff_secs(attempt: u32) -> u64 {
    let doublings = attempt.saturating_sub(1).min(32);
    BACKOFF_BASE_SECS
        .saturating_mul(1u64 << doublings)
        .min(BACKOFF_MAX_SECS)
}

/// The columns the state machine reads (times are unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobState {
    pub phase: Phase,
    pub origin: Origin,
    pub attempt: u32,
    pub next_attempt_at: u64,
    pub lease_expires_at: Option<u64>,
}

/// What a driver should do with a job it has just read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// Run the claim `UPDATE`; on success the driver owns `attempt`.
    Claim { attempt: u32 },
    /// The previous attempt was the last one and it never finished (crash or lost
    /// lease): fail the job with [`RETRIES_EXHAUSTED`] instead of claiming it.
    Exhausted,
    /// Another driver holds a live lease.
    Leased,
    /// Backing off; `next_attempt_at` is in the future.
    NotDue,
    /// `ready` or `failed`: nothing to do (a duplicate delivery after completion).
    Terminal,
}

pub fn plan_claim(s: &JobState, now: u64) -> Claim {
    if s.phase.is_terminal() {
        Claim::Terminal
    } else if s.lease_expires_at.is_some_and(|l| l > now) {
        Claim::Leased
    } else if s.next_attempt_at > now {
        Claim::NotDue
    } else if s.attempt >= MAX_ATTEMPTS {
        Claim::Exhausted
    } else {
        Claim::Claim {
            attempt: s.attempt + 1,
        }
    }
}

/// How a phase step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepError {
    /// Deterministic: retrying cannot help (bad manifest, missing revision, ...).
    Permanent(&'static str),
    /// Infrastructure: D1 unavailable, a subrequest failed, ...
    Transient(&'static str),
}

/// The conditional write a driver performs after a step. Every variant is applied
/// with `WHERE phase = from AND attempt = <owned attempt>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Advance {
        from: Phase,
        to: Phase,
        /// New lease expiry (refreshed on every non-terminal advance).
        lease_expires_at: Option<u64>,
    },
    Fail {
        from: Phase,
        code: &'static str,
        /// The transient error behind [`RETRIES_EXHAUSTED`], if that is the code.
        last_error: Option<&'static str>,
    },
    Retry {
        from: Phase,
        last_error: &'static str,
        next_attempt_at: u64,
    },
}

impl Transition {
    pub fn from(&self) -> Phase {
        match *self {
            Transition::Advance { from, .. }
            | Transition::Fail { from, .. }
            | Transition::Retry { from, .. } => from,
        }
    }

    /// The phase the row is in after the write.
    pub fn to(&self) -> Phase {
        match *self {
            Transition::Advance { to, .. } => to,
            Transition::Fail { .. } => Phase::Failed,
            Transition::Retry { from, .. } => from,
        }
    }

    /// Whether the owning driver keeps going after applying it.
    pub fn continues(&self) -> bool {
        matches!(self, Transition::Advance { to, .. } if !to.is_terminal())
    }
}

/// Decide the write after the step for `phase` ran under `attempt` (the claim token).
pub fn on_step(
    phase: Phase,
    origin: Origin,
    attempt: u32,
    result: Result<(), StepError>,
    now: u64,
) -> Transition {
    match result {
        Ok(()) => match next_phase(phase, origin) {
            Some(to) => Transition::Advance {
                from: phase,
                to,
                lease_expires_at: (!to.is_terminal()).then(|| now.saturating_add(LEASE_SECS)),
            },
            // No forward edge (a CLI job found in `compiling`, which no write path
            // produces): fail loudly instead of looping.
            None => Transition::Fail {
                from: phase,
                code: "illegal_phase",
                last_error: None,
            },
        },
        Err(StepError::Permanent(code)) => Transition::Fail {
            from: phase,
            code,
            last_error: None,
        },
        Err(StepError::Transient(code)) if attempt >= MAX_ATTEMPTS => Transition::Fail {
            from: phase,
            code: RETRIES_EXHAUSTED,
            last_error: Some(code),
        },
        Err(StepError::Transient(code)) => Transition::Retry {
            from: phase,
            last_error: code,
            next_attempt_at: now.saturating_add(backoff_secs(attempt)),
        },
    }
}

/// The `WHERE phase = ? AND attempt = ?` guard every transition shares.
pub fn guard_matches(row: &JobState, from: Phase, attempt: u32) -> bool {
    row.phase == from && row.attempt == attempt
}

/// The claim `UPDATE`'s full predicate: the guard, plus "not terminal, due, and not
/// leased", re-evaluated in SQL at write time (never trusted from the earlier read).
pub fn claim_matches(
    row: &JobState,
    expected_phase: Phase,
    expected_attempt: u32,
    now: u64,
) -> bool {
    guard_matches(row, expected_phase, expected_attempt)
        && matches!(plan_claim(row, now), Claim::Claim { .. })
}

/// In-memory model of the conditional writes (the SQL in [`super::sql`] implements
/// the same predicates; `tests/unit/studio-publish-sql.test.mjs` proves it against
/// SQLite). Returns `false`, changing nothing, when the guard does not match.
pub fn apply_claim(
    row: &mut JobState,
    expected_phase: Phase,
    expected_attempt: u32,
    now: u64,
) -> bool {
    if !claim_matches(row, expected_phase, expected_attempt, now) {
        return false;
    }
    row.attempt += 1;
    row.lease_expires_at = Some(now.saturating_add(LEASE_SECS));
    true
}

/// See [`apply_claim`].
pub fn apply_transition(row: &mut JobState, t: &Transition, attempt: u32) -> bool {
    if row.phase.is_terminal() || !guard_matches(row, t.from(), attempt) {
        return false;
    }
    match *t {
        Transition::Advance {
            to,
            lease_expires_at,
            ..
        } => {
            row.phase = to;
            row.lease_expires_at = lease_expires_at;
        }
        Transition::Fail { .. } => {
            row.phase = Phase::Failed;
            row.lease_expires_at = None;
        }
        Transition::Retry {
            next_attempt_at, ..
        } => {
            row.next_attempt_at = next_attempt_at;
            row.lease_expires_at = None;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_795_737_600; // 2026-11-27T00:00:00Z

    fn queued(origin: Origin) -> JobState {
        JobState {
            phase: Phase::Queued,
            origin,
            attempt: 0,
            next_attempt_at: T0,
            lease_expires_at: None,
        }
    }

    /// One driver run over `row` (the algorithm `drive.rs` performs against D1):
    /// read, claim, then step until the job stops. `steps` decides each phase's result.
    fn drive(
        row: &mut JobState,
        snapshot: JobState,
        now: u64,
        mut steps: impl FnMut(Phase) -> Result<(), StepError>,
    ) -> Option<Vec<Phase>> {
        let Claim::Claim { attempt } = plan_claim(&snapshot, now) else {
            return None;
        };
        if !apply_claim(row, snapshot.phase, snapshot.attempt, now) {
            return None;
        }
        let mut seen = vec![row.phase];
        let mut phase = row.phase;
        loop {
            let t = on_step(phase, row.origin, attempt, steps(phase), now);
            if !apply_transition(row, &t, attempt) {
                return Some(seen);
            }
            phase = t.to();
            seen.push(phase);
            if !t.continues() {
                return Some(seen);
            }
        }
    }

    /// [`drive`] over a fresh read of the row.
    fn run(
        row: &mut JobState,
        now: u64,
        steps: impl FnMut(Phase) -> Result<(), StepError>,
    ) -> Option<Vec<Phase>> {
        let snapshot = *row;
        drive(row, snapshot, now, steps)
    }

    #[test]
    fn phase_and_origin_strings_round_trip() {
        for p in Phase::ALL {
            assert_eq!(Phase::parse(p.as_str()), Some(*p));
        }
        for o in Origin::ALL {
            assert_eq!(Origin::parse(o.as_str()), Some(*o));
        }
        assert_eq!(Phase::parse("Ready"), None);
        assert_eq!(Phase::parse("succeeded"), None);
        assert_eq!(Origin::parse("CLI"), None);
        assert!(Phase::Ready.is_terminal() && Phase::Failed.is_terminal());
        for p in [
            Phase::Queued,
            Phase::Validating,
            Phase::Compiling,
            Phase::Uploading,
        ] {
            assert!(!p.is_terminal(), "{p:?}");
        }
    }

    #[test]
    fn forward_edges_per_origin() {
        use Phase::*;
        assert_eq!(next_phase(Queued, Origin::Cli), Some(Validating));
        assert_eq!(next_phase(Queued, Origin::Server), Some(Compiling));
        assert_eq!(next_phase(Compiling, Origin::Server), Some(Validating));
        for o in Origin::ALL {
            assert_eq!(next_phase(Validating, *o), Some(Uploading));
            assert_eq!(next_phase(Uploading, *o), Some(Ready));
            assert_eq!(next_phase(Ready, *o), None);
            assert_eq!(next_phase(Failed, *o), None);
        }
        // A CLI artifact never passes through the server compiler.
        assert_eq!(next_phase(Compiling, Origin::Cli), None);
    }

    #[test]
    fn every_legal_transition_and_no_illegal_one() {
        use Phase::*;
        let legal: &[(Phase, Phase, Origin)] = &[
            (Queued, Validating, Origin::Cli),
            (Queued, Compiling, Origin::Server),
            (Compiling, Validating, Origin::Server),
            (Validating, Uploading, Origin::Cli),
            (Validating, Uploading, Origin::Server),
            (Uploading, Ready, Origin::Cli),
            (Uploading, Ready, Origin::Server),
        ];
        for from in Phase::ALL {
            for to in Phase::ALL {
                for o in Origin::ALL {
                    let expect =
                        legal.contains(&(*from, *to, *o)) || (*to == Failed && !from.is_terminal());
                    assert_eq!(
                        can_transition(*from, *to, *o),
                        expect,
                        "{from:?} -> {to:?} ({o:?})"
                    );
                }
            }
        }
        // Spot checks of the forbidden ones.
        assert!(
            !can_transition(Ready, Failed, Origin::Cli),
            "terminal stays terminal"
        );
        assert!(
            !can_transition(Failed, Queued, Origin::Cli),
            "no resurrection"
        );
        assert!(
            !can_transition(Queued, Ready, Origin::Cli),
            "no skipping validation"
        );
        assert!(!can_transition(Queued, Uploading, Origin::Cli));
        assert!(
            !can_transition(Uploading, Validating, Origin::Cli),
            "no going back"
        );
        assert!(!can_transition(Queued, Compiling, Origin::Cli));
        assert!(!can_transition(Validating, Validating, Origin::Cli));
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(backoff_secs(1), 30);
        assert_eq!(backoff_secs(2), 60);
        assert_eq!(backoff_secs(3), 120);
        assert_eq!(backoff_secs(4), 240);
        assert_eq!(backoff_secs(7), 1920);
        assert_eq!(backoff_secs(8), BACKOFF_MAX_SECS);
        assert_eq!(backoff_secs(u32::MAX), BACKOFF_MAX_SECS, "no overflow");
        assert_eq!(
            backoff_secs(0),
            BACKOFF_BASE_SECS,
            "attempt 0 is treated as the first"
        );
        for a in 1..40 {
            assert!(backoff_secs(a) <= backoff_secs(a + 1), "monotonic at {a}");
        }
    }

    #[test]
    fn claim_plan() {
        let q = queued(Origin::Cli);
        assert_eq!(plan_claim(&q, T0), Claim::Claim { attempt: 1 });
        assert_eq!(plan_claim(&q, T0 - 1), Claim::NotDue);
        let leased = JobState {
            phase: Phase::Validating,
            attempt: 1,
            lease_expires_at: Some(T0 + 10),
            ..q
        };
        assert_eq!(plan_claim(&leased, T0), Claim::Leased);
        assert_eq!(plan_claim(&leased, T0 + 9), Claim::Leased);
        // The lease is over AT its expiry second.
        assert_eq!(plan_claim(&leased, T0 + 10), Claim::Claim { attempt: 2 });
        for p in [Phase::Ready, Phase::Failed] {
            let done = JobState { phase: p, ..leased };
            assert_eq!(plan_claim(&done, T0 + 1_000_000), Claim::Terminal);
        }
        let last = JobState {
            attempt: MAX_ATTEMPTS,
            lease_expires_at: Some(T0),
            ..leased
        };
        assert_eq!(plan_claim(&last, T0), Claim::Exhausted);
        let below = JobState {
            attempt: MAX_ATTEMPTS - 1,
            ..last
        };
        assert_eq!(
            plan_claim(&below, T0),
            Claim::Claim {
                attempt: MAX_ATTEMPTS
            }
        );
    }

    #[test]
    fn step_outcomes() {
        // Success advances along the forward edge and refreshes the lease.
        assert_eq!(
            on_step(Phase::Validating, Origin::Cli, 1, Ok(()), T0),
            Transition::Advance {
                from: Phase::Validating,
                to: Phase::Uploading,
                lease_expires_at: Some(T0 + LEASE_SECS)
            }
        );
        // Reaching a terminal phase releases the lease.
        assert_eq!(
            on_step(Phase::Uploading, Origin::Cli, 1, Ok(()), T0),
            Transition::Advance {
                from: Phase::Uploading,
                to: Phase::Ready,
                lease_expires_at: None
            }
        );
        // Permanent failures keep their phase-specific code, whatever the attempt.
        assert_eq!(
            on_step(
                Phase::Validating,
                Origin::Cli,
                1,
                Err(StepError::Permanent("revision_missing")),
                T0
            ),
            Transition::Fail {
                from: Phase::Validating,
                code: "revision_missing",
                last_error: None
            }
        );
        assert_eq!(
            on_step(
                Phase::Compiling,
                Origin::Server,
                1,
                Err(StepError::Permanent("server_compile_disabled")),
                T0
            ),
            Transition::Fail {
                from: Phase::Compiling,
                code: "server_compile_disabled",
                last_error: None
            }
        );
        // Transient failures stay in place and back off...
        assert_eq!(
            on_step(
                Phase::Uploading,
                Origin::Cli,
                2,
                Err(StepError::Transient("d1_unavailable")),
                T0
            ),
            Transition::Retry {
                from: Phase::Uploading,
                last_error: "d1_unavailable",
                next_attempt_at: T0 + 60
            }
        );
        // ...until the last attempt, which fails with the exhaustion code and keeps the cause.
        assert_eq!(
            on_step(
                Phase::Uploading,
                Origin::Cli,
                MAX_ATTEMPTS,
                Err(StepError::Transient("d1_unavailable")),
                T0
            ),
            Transition::Fail {
                from: Phase::Uploading,
                code: RETRIES_EXHAUSTED,
                last_error: Some("d1_unavailable")
            }
        );
    }

    #[test]
    fn transitions_only_follow_legal_edges() {
        for from in Phase::ALL.iter().filter(|p| !p.is_terminal()) {
            for o in Origin::ALL {
                if next_phase(*from, *o).is_none() {
                    continue;
                }
                for r in [
                    Ok(()),
                    Err(StepError::Permanent("x")),
                    Err(StepError::Transient("y")),
                ] {
                    for attempt in [1, MAX_ATTEMPTS] {
                        let t = on_step(*from, *o, attempt, r, T0);
                        assert_eq!(t.from(), *from);
                        assert!(
                            t.to() == *from || can_transition(*from, t.to(), *o),
                            "{t:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn guards_match_only_the_exact_phase_and_attempt() {
        let row = JobState {
            phase: Phase::Validating,
            attempt: 2,
            ..queued(Origin::Cli)
        };
        assert!(guard_matches(&row, Phase::Validating, 2));
        assert!(!guard_matches(&row, Phase::Validating, 1), "stale attempt");
        assert!(!guard_matches(&row, Phase::Queued, 2), "stale phase");
        assert!(!guard_matches(&row, Phase::Uploading, 2));
    }

    #[test]
    fn happy_path_cli_reaches_ready_in_one_run() {
        let mut row = queued(Origin::Cli);
        let seen = run(&mut row, T0, |_| Ok(())).unwrap();
        assert_eq!(
            seen,
            vec![
                Phase::Queued,
                Phase::Validating,
                Phase::Uploading,
                Phase::Ready
            ]
        );
        assert_eq!(row.phase, Phase::Ready);
        assert_eq!(row.attempt, 1);
        assert_eq!(
            row.lease_expires_at, None,
            "the lease is released at the end"
        );
    }

    #[test]
    fn server_jobs_fail_in_compiling_while_it_is_unimplemented() {
        let mut row = queued(Origin::Server);
        let seen = run(&mut row, T0, |p| match p {
            Phase::Compiling => Err(StepError::Permanent("server_compile_disabled")),
            _ => Ok(()),
        })
        .unwrap();
        assert_eq!(seen, vec![Phase::Queued, Phase::Compiling, Phase::Failed]);
        assert_eq!(row.phase, Phase::Failed);
    }

    #[test]
    fn duplicate_delivery_is_a_no_op() {
        // Two deliveries of the same pointer read the same row before either writes.
        let mut row = queued(Origin::Cli);
        let snapshot = row;
        let first = drive(&mut row, snapshot, T0, |_| Ok(()));
        assert!(first.is_some());
        let after_first = row;
        let second = drive(&mut row, snapshot, T0, |_| {
            panic!("the duplicate must not run a step")
        });
        assert_eq!(
            second, None,
            "stale (phase, attempt): the claim matches nothing"
        );
        assert_eq!(row, after_first, "the duplicate changed nothing");
        // A third delivery that re-reads the row sees a terminal job.
        assert_eq!(plan_claim(&row, T0), Claim::Terminal);
    }

    #[test]
    fn interleaved_drivers_cannot_both_own_a_job() {
        let mut row = queued(Origin::Cli);
        let snap = row;
        // Driver A claims.
        assert!(apply_claim(&mut row, snap.phase, snap.attempt, T0));
        let a = row.attempt;
        // Driver B, with the same snapshot, loses the claim.
        assert!(!apply_claim(&mut row, snap.phase, snap.attempt, T0));
        // Even a fresh re-read by B is refused while A's lease is live.
        let reread = row;
        assert_eq!(plan_claim(&reread, T0 + 1), Claim::Leased);
        assert!(!apply_claim(&mut row, reread.phase, reread.attempt, T0 + 1));
        // After A's lease expires (A is slow or dead), B reclaims with a NEW token...
        let late = T0 + LEASE_SECS;
        assert!(apply_claim(&mut row, reread.phase, reread.attempt, late));
        let b = row.attempt;
        assert_eq!(b, a + 1);
        // ...and A's late write is rejected: its token is stale.
        let t = on_step(Phase::Queued, Origin::Cli, a, Ok(()), late);
        assert!(!apply_transition(&mut row, &t, a));
        assert_eq!(row.phase, Phase::Queued);
        let t = on_step(Phase::Queued, Origin::Cli, b, Ok(()), late);
        assert!(apply_transition(&mut row, &t, b));
        assert_eq!(row.phase, Phase::Validating);
    }

    #[test]
    fn crash_between_phases_resumes_from_the_reached_phase() {
        let mut row = queued(Origin::Cli);
        // Run 1 claims, completes `queued` and `validating`, then the isolate dies.
        let snap = row;
        assert!(apply_claim(&mut row, snap.phase, snap.attempt, T0));
        for p in [Phase::Queued, Phase::Validating] {
            let t = on_step(p, Origin::Cli, 1, Ok(()), T0);
            assert!(apply_transition(&mut row, &t, 1));
        }
        assert_eq!(row.phase, Phase::Uploading);
        assert_eq!(row.lease_expires_at, Some(T0 + LEASE_SECS));
        // The next scheduled run inside the lease leaves it alone.
        assert_eq!(run(&mut row, T0 + 60, |_| Ok(())), None);
        assert_eq!(row.phase, Phase::Uploading);
        // After expiry it resumes at `uploading` (validation is not repeated).
        let mut ran = vec![];
        let seen = run(&mut row, T0 + LEASE_SECS, |p| {
            ran.push(p);
            Ok(())
        })
        .unwrap();
        assert_eq!(ran, vec![Phase::Uploading]);
        assert_eq!(seen, vec![Phase::Uploading, Phase::Ready]);
        assert_eq!(row.attempt, 2);
    }

    #[test]
    fn transient_failures_back_off_then_exhaust_with_the_phase_kept() {
        let mut row = queued(Origin::Cli);
        let mut now = T0;
        for attempt in 1..MAX_ATTEMPTS {
            let seen = run(&mut row, now, |p| match p {
                Phase::Validating => Err(StepError::Transient("d1_unavailable")),
                _ => Ok(()),
            })
            .unwrap();
            assert_eq!(*seen.last().unwrap(), Phase::Validating);
            assert_eq!(row.attempt, attempt);
            assert_eq!(row.phase, Phase::Validating);
            assert_eq!(row.lease_expires_at, None, "a retry releases the lease");
            assert_eq!(row.next_attempt_at, now + backoff_secs(attempt));
            // Not due before the backoff elapses.
            assert_eq!(
                {
                    let at = row.next_attempt_at - 1;
                    run(&mut row, at, |_| Ok(()))
                },
                None
            );
            now = row.next_attempt_at;
        }
        // The last attempt fails the job for good, in the phase where it happened.
        run(&mut row, now, |_| {
            Err(StepError::Transient("d1_unavailable"))
        })
        .unwrap();
        assert_eq!(row.phase, Phase::Failed);
        assert_eq!(row.attempt, MAX_ATTEMPTS);
        assert_eq!(plan_claim(&row, now + 10_000), Claim::Terminal);
    }

    #[test]
    fn a_crash_on_the_last_attempt_is_exhausted_not_reclaimed() {
        let row = JobState {
            phase: Phase::Uploading,
            attempt: MAX_ATTEMPTS,
            lease_expires_at: Some(T0),
            ..queued(Origin::Cli)
        };
        assert_eq!(plan_claim(&row, T0), Claim::Exhausted);
        let mut r = row;
        assert!(
            !apply_claim(&mut r, row.phase, row.attempt, T0),
            "never claimed past the cap"
        );
        let t = Transition::Fail {
            from: Phase::Uploading,
            code: RETRIES_EXHAUSTED,
            last_error: Some("lease_expired"),
        };
        assert!(apply_transition(&mut r, &t, MAX_ATTEMPTS));
        assert_eq!(r.phase, Phase::Failed);
    }

    #[test]
    fn terminal_rows_reject_every_write() {
        for p in [Phase::Ready, Phase::Failed] {
            let mut row = JobState {
                phase: p,
                attempt: 1,
                ..queued(Origin::Cli)
            };
            let before = row;
            assert!(!apply_claim(&mut row, p, 1, T0 + 10_000));
            for t in [
                Transition::Fail {
                    from: p,
                    code: "x",
                    last_error: None,
                },
                Transition::Retry {
                    from: p,
                    last_error: "x",
                    next_attempt_at: T0,
                },
            ] {
                assert!(!apply_transition(&mut row, &t, 1), "{t:?}");
            }
            assert_eq!(row, before);
        }
    }
}
