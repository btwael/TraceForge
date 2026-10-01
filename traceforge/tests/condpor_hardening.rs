//! Adversarial hardening tests for the ConDPOR symbolic implementation.
//!
//! Every test names the property it would falsify (soundness, completeness or
//! optimality) and states the hand-derived expectation above the assertion.
//! The expectations were derived before any of these tests was executed.
#![cfg(feature = "symbolic")]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use traceforge::monitor_types::{Acceptor, Monitor, MonitorResult, Observer};
use traceforge::{
    recv_msg, recv_msg_block, send_msg, symbolic, thread, verify, Config, SchedulePolicy,
};
use traceforge_macros::monitor;

fn ltr() -> Config {
    Config::builder()
        .with_policy(SchedulePolicy::LTR)
        .with_symbolic(true)
        .build()
}

// ---------------------------------------------------------------------------
// S1 / S2 — the pre-fix unsatisfiable execution
// ---------------------------------------------------------------------------

/// SOUNDNESS. `x > 0` and `x <= 0` are complementary, so no execution of this
/// program can observe both as true. The deleted-constraint thread is spawned
/// first so that, under LTR, it re-appends its (new) constraint event *before*
/// the retained constraint of the second thread replays.
///
/// Derived behaviours: (x>0, x<=0) in {(T,F),(F,T)}, each with the receive
/// reading nothing or reading the send => 4 graphs, 0 blocked.
#[test]
fn condpor_s1_retained_constraint_forbids_contradictory_execution() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let main_id = thread::current_id();
        let x = symbolic::fresh_int();

        let deleted = Arc::new(Mutex::new(None));
        let retained = Arc::new(Mutex::new(None));

        let d_in = deleted.clone();
        let x_d = x.clone();
        let t_deleted = thread::spawn(move || {
            *d_in.lock().unwrap() = Some(symbolic::eval(x_d.gt(0)));
        });

        let r_in = retained.clone();
        let t_retained = thread::spawn(move || {
            *r_in.lock().unwrap() = Some(symbolic::eval(x.le(0)));
            send_msg(main_id, 1_i32);
        });

        let got = recv_msg::<i32>().is_some();
        t_deleted.join().unwrap();
        t_retained.join().unwrap();

        let d = (*deleted.lock().unwrap()).unwrap();
        let r = (*retained.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((d, r, got));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        !seen.iter().any(|(d, r, _)| *d && *r),
        "SOUNDNESS VIOLATION: an execution observed both x > 0 and x <= 0: {seen:?}"
    );
    assert_eq!(
        seen,
        HashSet::from([
            (true, false, false),
            (true, false, true),
            (false, true, false),
            (false, true, true),
        ])
    );
    assert_eq!((stats.execs, stats.block), (4, 0));
    assert_eq!(stats.execs, seen.len());
}

/// SOUNDNESS, and a user-reachable panic. Same shape as S1, but `main`
/// evaluates a third constraint *after* joining both threads, i.e. at a point
/// where the whole path condition is in the solver. If an execution ever holds
/// `x > 0 && x <= 0`, that third evaluation finds both its branches
/// unsatisfiable and `handle_constraint_eval` panics.
///
/// Derived feasible assignments of (x>0, x<=0, x>100):
/// (T,F,T), (T,F,F), (F,T,F) — three symbolic paths; the independent receive
/// doubles each => 6 graphs, 0 blocked.
#[test]
fn condpor_s2_unsat_path_condition_is_never_evaluated_against() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let main_id = thread::current_id();
        let x = symbolic::fresh_int();

        let deleted = Arc::new(Mutex::new(None));
        let retained = Arc::new(Mutex::new(None));

        let d_in = deleted.clone();
        let x_d = x.clone();
        let t_deleted = thread::spawn(move || {
            *d_in.lock().unwrap() = Some(symbolic::eval(x_d.gt(0)));
        });

        let r_in = retained.clone();
        let x_r = x.clone();
        let t_retained = thread::spawn(move || {
            *r_in.lock().unwrap() = Some(symbolic::eval(x_r.le(0)));
            send_msg(main_id, 1_i32);
        });

        let got = recv_msg::<i32>().is_some();
        t_deleted.join().unwrap();
        t_retained.join().unwrap();

        // Evaluated with the full path condition of this graph in the solver.
        let big = symbolic::eval(x.gt(100));

        let d = (*deleted.lock().unwrap()).unwrap();
        let r = (*retained.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((d, r, big, got));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        !seen.iter().any(|(d, r, _, _)| *d && *r),
        "SOUNDNESS VIOLATION: {seen:?}"
    );
    assert_eq!(
        seen,
        HashSet::from([
            (true, false, true, false),
            (true, false, true, true),
            (true, false, false, false),
            (true, false, false, true),
            (false, true, false, false),
            (false, true, false, true),
        ])
    );
    assert_eq!((stats.execs, stats.block), (6, 0));
    assert_eq!(stats.execs, seen.len());
}

/// SOUNDNESS. Three threads, two independent variables, and a third constraint
/// that is *implied* by the other two: `t3` evaluates `(x>0) == (y>0)`, whose
/// value is forced once both earlier branches are fixed. If the path condition
/// in force when `t3`'s event is (re-)added omits either earlier constraint,
/// `t3` is free to choose `true` and the recorded triple becomes inconsistent.
///
/// Derived: (x>0, y>0) free and independent => 4 symbolic paths, each with
/// e3 == (e1 == e2); the independent receive doubles each => 8 graphs, 0 blocked.
#[test]
fn condpor_s3_implied_constraint_is_forced_across_a_backward_revisit() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let main_id = thread::current_id();
        let x = symbolic::fresh_int();
        let y = symbolic::fresh_int();

        let e1 = Arc::new(Mutex::new(None));
        let e2 = Arc::new(Mutex::new(None));
        let e3 = Arc::new(Mutex::new(None));

        let e1_in = e1.clone();
        let x1 = x.clone();
        let t1 = thread::spawn(move || {
            *e1_in.lock().unwrap() = Some(symbolic::eval(x1.gt(0)));
        });

        let e2_in = e2.clone();
        let y2 = y.clone();
        let t2 = thread::spawn(move || {
            *e2_in.lock().unwrap() = Some(symbolic::eval(y2.gt(0)));
            send_msg(main_id, 1_i32);
        });

        let e3_in = e3.clone();
        let t3 = thread::spawn(move || {
            *e3_in.lock().unwrap() = Some(symbolic::eval(x.gt(0).equals(y.gt(0))));
        });

        let got = recv_msg::<i32>().is_some();
        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        let a = (*e1.lock().unwrap()).unwrap();
        let b = (*e2.lock().unwrap()).unwrap();
        let c = (*e3.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((a, b, c, got));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        seen.iter().all(|(a, b, c, _)| *c == (a == b)),
        "SOUNDNESS VIOLATION: an execution observed (x>0)==(y>0) inconsistently: {seen:?}"
    );
    let mut expected = HashSet::new();
    for a in [false, true] {
        for b in [false, true] {
            for got in [false, true] {
                expected.insert((a, b, a == b, got));
            }
        }
    }
    assert_eq!(seen, expected);
    assert_eq!((stats.execs, stats.block), (8, 0));
    assert_eq!(stats.execs, seen.len());
}

// ---------------------------------------------------------------------------
// Item 1 — the owner's three-graph optimality test, and its mutations
// ---------------------------------------------------------------------------

/// OPTIMALITY / COMPLETENESS. The owner's test, compiled to message passing.
///
/// ```text
/// main: x := nondet(); spawn(T1, T2)
/// T1:   a := y;  eval(x > 0)
/// T2:   if eval(x > 0) then no-op else y := 1
/// ```
///
/// Derivation (checked, not trusted):
/// - x>0 true: T1's eval is the first evaluation, both branches feasible, it
///   takes true and pushes a forward revisit; T2's eval is then forced true, so
///   T2 does not send and T1's receive reads nothing. 1 graph.
/// - x>0 false: T2's eval is forced false and T2 sends; T1's receive reads
///   nothing or reads that send. 2 graphs.
///
/// Total 3 graphs, 0 blocked.
#[test]
fn condpor_owner_three_graph_test() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();

        let t1_obs = Arc::new(Mutex::new(None));
        let t2_obs = Arc::new(Mutex::new(None));

        let t1_in = t1_obs.clone();
        let x1 = x.clone();
        let t1 = thread::spawn(move || {
            let a: Option<i32> = recv_msg();
            let b = symbolic::eval(x1.gt(0));
            *t1_in.lock().unwrap() = Some((a.is_some(), b));
        });
        let t1_id = t1.thread().id();

        let t2_in = t2_obs.clone();
        let t2 = thread::spawn(move || {
            let b = symbolic::eval(x.gt(0));
            if b {
                // no-op
            } else {
                send_msg(t1_id, 1_i32);
            }
            *t2_in.lock().unwrap() = Some(b);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let (a, b1) = (*t1_obs.lock().unwrap()).unwrap();
        let b2 = (*t2_obs.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((a, b1, b2));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        seen.iter().all(|(_, b1, b2)| b1 == b2),
        "SOUNDNESS VIOLATION: the same constraint got two values: {seen:?}"
    );
    assert_eq!(
        seen,
        HashSet::from([
            (false, true, true),
            (false, false, false),
            (true, false, false),
        ])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
}

/// Mutation A of the owner's test: T1 evaluates *before* receiving. The
/// constraint then sits at a lower stamp than the receive, so the backward
/// revisit deletes no constraint at all — a structurally different path through
/// `is_maximal_extension`. Derived count is unchanged: 3 graphs, 0 blocked.
#[test]
fn condpor_owner_mutation_a_eval_before_receive() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();

        let t1_obs = Arc::new(Mutex::new(None));
        let t2_obs = Arc::new(Mutex::new(None));

        let t1_in = t1_obs.clone();
        let x1 = x.clone();
        let t1 = thread::spawn(move || {
            let b = symbolic::eval(x1.gt(0));
            let a: Option<i32> = recv_msg();
            *t1_in.lock().unwrap() = Some((a.is_some(), b));
        });
        let t1_id = t1.thread().id();

        let t2_in = t2_obs.clone();
        let t2 = thread::spawn(move || {
            let b = symbolic::eval(x.gt(0));
            if !b {
                send_msg(t1_id, 1_i32);
            }
            *t2_in.lock().unwrap() = Some(b);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let (a, b1) = (*t1_obs.lock().unwrap()).unwrap();
        let b2 = (*t2_obs.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((a, b1, b2));
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(
        seen,
        HashSet::from([
            (false, true, true),
            (false, false, false),
            (true, false, false),
        ])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
}

/// Mutation B of the owner's test: T2's evaluation is hoisted out of the `if`
/// condition into a `let`. This is event-identical to the original (the same
/// single `ConstraintEval` at the same position), so the derived count must be
/// unchanged: 3 graphs, 0 blocked. Recorded to confirm the mutation is inert.
#[test]
fn condpor_owner_mutation_b_hoisted_eval() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();

        let t1_obs = Arc::new(Mutex::new(None));
        let t2_obs = Arc::new(Mutex::new(None));

        let t1_in = t1_obs.clone();
        let x1 = x.clone();
        let t1 = thread::spawn(move || {
            let a: Option<i32> = recv_msg();
            let b = symbolic::eval(x1.gt(0));
            *t1_in.lock().unwrap() = Some((a.is_some(), b));
        });
        let t1_id = t1.thread().id();

        let t2_in = t2_obs.clone();
        let t2 = thread::spawn(move || {
            let positive = symbolic::eval(x.gt(0));
            if positive {
                // no-op
            } else {
                send_msg(t1_id, 1_i32);
            }
            *t2_in.lock().unwrap() = Some(positive);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let (a, b1) = (*t1_obs.lock().unwrap()).unwrap();
        let b2 = (*t2_obs.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((a, b1, b2));
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(
        seen,
        HashSet::from([
            (false, true, true),
            (false, false, false),
            (true, false, false),
        ])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
}

/// Mutation C of the owner's test: T2 sends twice on the false branch.
///
/// **My first derivation was wrong and is recorded here unchanged.** I derived:
/// x>0 true => 1 graph (no sends); x>0 false => the receive reads nothing, the
/// first send, or the second send => 3 graphs; total 4 graphs. Measurement gave
/// 3 graphs, with `Some(2)` never observed.
///
/// The cause is not a missing graph: `Config`'s default `ConsType` is **FIFO**
/// (`lib.rs:297`), under which a single receive on a FIFO channel cannot take
/// the second message while the first is unread. Under FIFO the correct derived
/// count is 3. `condpor_owner_mutation_c_two_sends_bag` re-derives 4 under
/// `ConsType::Bag` and confirms the explanation rather than assuming it.
#[test]
fn condpor_owner_mutation_c_two_sends() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();

        let t1_obs = Arc::new(Mutex::new(None));
        let t2_obs = Arc::new(Mutex::new(None));

        let t1_in = t1_obs.clone();
        let x1 = x.clone();
        let t1 = thread::spawn(move || {
            let a: Option<i32> = recv_msg();
            let b = symbolic::eval(x1.gt(0));
            *t1_in.lock().unwrap() = Some((a, b));
        });
        let t1_id = t1.thread().id();

        let t2_in = t2_obs.clone();
        let t2 = thread::spawn(move || {
            let positive = symbolic::eval(x.gt(0));
            if !positive {
                send_msg(t1_id, 1_i32);
                send_msg(t1_id, 2_i32);
            }
            *t2_in.lock().unwrap() = Some(positive);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let (a, b1) = (*t1_obs.lock().unwrap()).unwrap();
        let b2 = (*t2_obs.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((a, b1, b2));
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(
        seen,
        HashSet::from([
            (None, true, true),
            (None, false, false),
            (Some(1), false, false),
        ])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
}

/// Mutation C under unordered channels. This is the independent check of the
/// explanation for the mutation-C disagreement: with FIFO removed, the second
/// send becomes readable.
///
/// Derived: x>0 true => 1 graph; x>0 false => the receive reads nothing, the
/// first send, or the second send => 3 graphs.
///
/// Total 4 graphs, 0 blocked.
#[test]
fn condpor_owner_mutation_c_two_sends_bag() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let cfg = Config::builder()
        .with_policy(SchedulePolicy::LTR)
        .with_cons_type(traceforge::ConsType::Bag)
        .with_symbolic(true)
        .build();

    let stats = verify(cfg, move || {
        let x = symbolic::fresh_int();

        let t1_obs = Arc::new(Mutex::new(None));
        let t2_obs = Arc::new(Mutex::new(None));

        let t1_in = t1_obs.clone();
        let x1 = x.clone();
        let t1 = thread::spawn(move || {
            let a: Option<i32> = recv_msg();
            let b = symbolic::eval(x1.gt(0));
            *t1_in.lock().unwrap() = Some((a, b));
        });
        let t1_id = t1.thread().id();

        let t2_in = t2_obs.clone();
        let t2 = thread::spawn(move || {
            let positive = symbolic::eval(x.gt(0));
            if !positive {
                send_msg(t1_id, 1_i32);
                send_msg(t1_id, 2_i32);
            }
            *t2_in.lock().unwrap() = Some(positive);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let (a, b1) = (*t1_obs.lock().unwrap()).unwrap();
        let b2 = (*t2_obs.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((a, b1, b2));
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(
        seen,
        HashSet::from([
            (None, true, true),
            (None, false, false),
            (Some(1), false, false),
            (Some(2), false, false),
        ])
    );
    assert_eq!((stats.execs, stats.block), (4, 0));
    assert_eq!(stats.execs, seen.len());
}

/// Mutation D of the owner's test: T1's receive is blocking. On the `x > 0`
/// true branch T2 never sends, so T1 blocks forever and T1's own evaluation is
/// never reached — the branch becomes a blocked execution decided by T2's
/// evaluation alone.
///
/// Derived: 1 complete graph (x<=0, T1 reads the send), 1 blocked graph.
#[test]
fn condpor_owner_mutation_d_blocking_receive() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();

        let t1_obs = Arc::new(Mutex::new(None));

        let t1_in = t1_obs.clone();
        let x1 = x.clone();
        let t1 = thread::spawn(move || {
            let a: i32 = recv_msg_block();
            let b = symbolic::eval(x1.gt(0));
            *t1_in.lock().unwrap() = Some((a, b));
        });
        let t1_id = t1.thread().id();

        let t2 = thread::spawn(move || {
            let positive = symbolic::eval(x.gt(0));
            if !positive {
                send_msg(t1_id, 1_i32);
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let (a, b1) = (*t1_obs.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((a, b1));
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(seen, HashSet::from([(1, false)]));
    assert_eq!((stats.execs, stats.block), (1, 1));
    assert_eq!(stats.execs, seen.len());
}

// ---------------------------------------------------------------------------
// Item 2 — each mechanism individually
// ---------------------------------------------------------------------------

/// Value generation alone. A `fresh_int()` that is never evaluated adds an
/// event and nothing else. Derived: 1 graph, 0 blocked.
#[test]
fn condpor_f1_unevaluated_generation_changes_nothing() {
    let stats = verify(ltr(), || {
        let _x = symbolic::fresh_int();
    });
    assert_eq!((stats.execs, stats.block), (1, 0));
}

/// COMPLETENESS. Two generations at different positions must be distinct
/// variables. If they collided, the second evaluation would be forced by the
/// first and only 2 graphs would be explored.
///
/// Derived: 4 graphs (2 x 2 independent outcomes), 0 blocked.
#[test]
fn condpor_f1b_two_generations_are_independent_variables() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let ids = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();
    let ids_in_verify = ids.clone();

    let stats = verify(ltr(), move || {
        let a = symbolic::fresh_int();
        let b = symbolic::fresh_int();
        ids_in_verify
            .lock()
            .unwrap()
            .insert((format!("{a:?}"), format!("{b:?}")));
        let p = symbolic::eval(a.gt(0));
        let q = symbolic::eval(b.gt(0));
        observed_in_verify.lock().unwrap().insert((p, q));
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(
        seen,
        HashSet::from([(true, true), (true, false), (false, true), (false, false)])
    );
    assert_eq!((stats.execs, stats.block), (4, 0));
    assert_eq!(stats.execs, seen.len());

    // The same two generations keep one identity each across all executions,
    // and the two identities differ from each other.
    let id_pairs = ids.lock().unwrap().clone();
    assert_eq!(id_pairs.len(), 1, "{id_pairs:?}");
    let (first, second) = id_pairs.into_iter().next().unwrap();
    assert_ne!(first, second);
}

/// A single evaluation: exactly two executions, both outcomes.
/// Derived: 2 graphs, 0 blocked.
#[test]
fn condpor_f2_single_evaluation_explores_both_outcomes() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();
        let b = symbolic::eval(x.gt(0));
        observed_in_verify.lock().unwrap().insert(b);
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(seen, HashSet::from([true, false]));
    assert_eq!((stats.execs, stats.block), (2, 0));
    assert_eq!(stats.execs, seen.len());
}

/// A forced evaluation. `eval(x>0)` then `eval(x>1)`.
///
/// Derived feasible assignments of (x>0, x>1): (T,T), (T,F), (F,F). (F,T) is
/// unsatisfiable. 3 graphs, 0 blocked. The first execution takes `true`
/// wherever `true` is feasible, so it is (T,T).
#[test]
fn condpor_f3_forced_evaluation_forward_order() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();
        let gt0 = symbolic::eval(x.clone().gt(0));
        let gt1 = symbolic::eval(x.gt(1));
        observed_in_verify.lock().unwrap().push((gt0, gt1));
    });

    let order = observed.lock().unwrap().clone();
    let seen: HashSet<_> = order.iter().cloned().collect();
    assert_eq!(
        seen,
        HashSet::from([(true, true), (true, false), (false, false)])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
    assert_eq!(order[0], (true, true));
}

/// The reverse order of F3: `eval(x>1)` then `eval(x>0)`. Re-expressed in the
/// canonical variable order (x>0, x>1) the feasible set is the *same* three
/// assignments and the count is the same 3. Because the search prefers `true`
/// whenever `true` is feasible, and (x>0 && x>1) is satisfiable, the first
/// execution is again (T,T) — the owner's expectation that the canonical first
/// outcome differs does not hold for this pair.
#[test]
fn condpor_f3b_forced_evaluation_reverse_order() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();
        let gt1 = symbolic::eval(x.clone().gt(1));
        let gt0 = symbolic::eval(x.gt(0));
        observed_in_verify.lock().unwrap().push((gt0, gt1));
    });

    let order = observed.lock().unwrap().clone();
    let seen: HashSet<_> = order.iter().cloned().collect();
    assert_eq!(
        seen,
        HashSet::from([(true, true), (true, false), (false, false)])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
    assert_eq!(order[0], (true, true));
}

/// An evaluation that is unsatisfiable on one side must not fork.
/// `assume(x>0)` then `eval(x>0)`.
///
/// Derived: the assume is itself an evaluation and *does* fork, so its false
/// branch yields one blocked execution. The second evaluation is forced true.
/// 1 complete graph, 1 blocked graph. The task's "one execution" is right about
/// `execs` and omits `block == 1`.
#[test]
fn condpor_f4_unsatisfiable_side_does_not_fork() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();
        symbolic::assume(x.clone().gt(0));
        let again = symbolic::eval(x.gt(0));
        observed_in_verify.lock().unwrap().insert(again);
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(seen, HashSet::from([true]));
    assert_eq!((stats.execs, stats.block), (1, 1));
}

/// COMPLETENESS. A constraint over a symbolic value that crossed an rf edge.
/// Two generators each send their own variable to one evaluator whose receive
/// is blocking, so the receive has two rf choices and each carries a different
/// variable.
///
/// Derived: 2 rf choices x 2 branch outcomes => 4 graphs, 0 blocked, and the
/// two observed variable identities are distinct.
#[test]
fn condpor_f6_constraint_over_a_received_variable() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let eval_obs = Arc::new(Mutex::new(None));
        let eval_in = eval_obs.clone();

        let evaluator = thread::spawn(move || {
            let v: symbolic::SymExpr = recv_msg_block();
            let id = format!("{v:?}");
            let b = symbolic::eval(v.gt(0));
            *eval_in.lock().unwrap() = Some((id, b));
        });
        let ev_id = evaluator.thread().id();

        let g1 = thread::spawn(move || {
            let a = symbolic::fresh_int();
            send_msg(ev_id, a);
        });
        let g2 = thread::spawn(move || {
            let b = symbolic::fresh_int();
            send_msg(ev_id, b);
        });

        g1.join().unwrap();
        g2.join().unwrap();
        evaluator.join().unwrap();

        let (id, b) = (*eval_obs.lock().unwrap()).clone().unwrap();
        observed_in_verify.lock().unwrap().insert((id, b));
    });

    let seen = observed.lock().unwrap().clone();
    let distinct_ids: HashSet<_> = seen.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(distinct_ids.len(), 2, "{seen:?}");
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert_eq!((stats.execs, stats.block), (4, 0));
    assert_eq!(stats.execs, seen.len());
}

/// Quantifiers. `eval(forall x. P(x))` then `eval(exists x. P(x))`.
///
/// Derived: the first is free (both branches feasible) and takes true; under
/// `forall x. P(x)` the existential is forced true because an uninterpreted
/// sort is non-empty, so no fork. Under `!forall x. P(x)` the existential is
/// free again. Feasible set {(T,T), (F,T), (F,F)}; (T,F) is unsatisfiable.
/// 3 graphs, 0 blocked.
#[test]
fn condpor_f7_forall_then_exists() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let node = symbolic::uninterpreted_sort("Node");
        let marked = symbolic::predicate("marked", std::slice::from_ref(&node));

        let all = symbolic::forall([("x", node.clone())], {
            let marked = marked.clone();
            move |vars| marked.apply([vars.get("x")])
        });
        let some = symbolic::exists([("x", node)], {
            let marked = marked.clone();
            move |vars| marked.apply([vars.get("x")])
        });

        let a = symbolic::eval(all);
        let b = symbolic::eval(some);
        observed_in_verify.lock().unwrap().insert((a, b));
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(
        seen,
        HashSet::from([(true, true), (false, true), (false, false)])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
}

/// Nested quantifiers. `eval(forall x. exists y. same(x,y))` then
/// `eval(exists y. forall x. same(x,y))`.
///
/// Derived: the second formula implies the first. So under the first being
/// true the second is free (fork), and under the first being false the second
/// is forced false. Feasible set {(T,T), (T,F), (F,F)}; (F,T) is
/// unsatisfiable. 3 graphs, 0 blocked.
#[test]
fn condpor_f7b_nested_quantifier_implication_is_respected() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let node = symbolic::uninterpreted_sort("Node");
        let same = symbolic::predicate("same", &[node.clone(), node.clone()]);

        let fa_ex = symbolic::forall([("x", node.clone())], {
            let same = same.clone();
            let node = node.clone();
            move |outer| {
                let same = same.clone();
                outer.exists([("y", node.clone())], move |inner| {
                    same.apply([inner.get("x"), inner.get("y")])
                })
            }
        });
        let ex_fa = symbolic::exists([("y", node.clone())], {
            let same = same.clone();
            let node = node.clone();
            move |outer| {
                let same = same.clone();
                outer.forall([("x", node.clone())], move |inner| {
                    same.apply([inner.get("x"), inner.get("y")])
                })
            }
        });

        let a = symbolic::eval(fa_ex);
        let b = symbolic::eval(ex_fa);
        observed_in_verify.lock().unwrap().insert((a, b));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        seen.iter().all(|(a, b)| !(*b && !*a)),
        "SOUNDNESS VIOLATION: exists-forall true while forall-exists false: {seen:?}"
    );
    assert_eq!(
        seen,
        HashSet::from([(true, true), (true, false), (false, false)])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
}

// ---------------------------------------------------------------------------
// Item 3 — integration, synthetic, adversarial
// ---------------------------------------------------------------------------

/// SOUNDNESS / OPTIMALITY. Two retained constraints and two deleted ones: the
/// sender evaluates `x<=0` and `y<=0` before its send (both in the send's
/// porf-prefix, both retained), while the first thread evaluates `x>0` and
/// `y>0` (both deleted by the backward revisit).
///
/// Derived: x>0 == !(x<=0) and y>0 == !(y<=0), so 4 symbolic paths; the
/// independent receive doubles each => 8 graphs, 0 blocked.
#[test]
fn condpor_i1_two_retained_and_two_deleted_constraints() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let main_id = thread::current_id();
        let x = symbolic::fresh_int();
        let y = symbolic::fresh_int();

        let del = Arc::new(Mutex::new(None));
        let ret = Arc::new(Mutex::new(None));

        let del_in = del.clone();
        let (xd, yd) = (x.clone(), y.clone());
        let t_del = thread::spawn(move || {
            let d1 = symbolic::eval(xd.gt(0));
            let d2 = symbolic::eval(yd.gt(0));
            *del_in.lock().unwrap() = Some((d1, d2));
        });

        let ret_in = ret.clone();
        let t_ret = thread::spawn(move || {
            let r1 = symbolic::eval(x.le(0));
            let r2 = symbolic::eval(y.le(0));
            *ret_in.lock().unwrap() = Some((r1, r2));
            send_msg(main_id, 1_i32);
        });

        let got = recv_msg::<i32>().is_some();
        t_del.join().unwrap();
        t_ret.join().unwrap();

        let (d1, d2) = (*del.lock().unwrap()).unwrap();
        let (r1, r2) = (*ret.lock().unwrap()).unwrap();
        observed_in_verify
            .lock()
            .unwrap()
            .insert((d1, d2, r1, r2, got));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        seen.iter()
            .all(|(d1, d2, r1, r2, _)| *d1 != *r1 && *d2 != *r2),
        "SOUNDNESS VIOLATION: complementary constraints agreed: {seen:?}"
    );
    let mut expected = HashSet::new();
    for d1 in [false, true] {
        for d2 in [false, true] {
            for got in [false, true] {
                expected.insert((d1, d2, !d1, !d2, got));
            }
        }
    }
    assert_eq!(seen, expected);
    assert_eq!((stats.execs, stats.block), (8, 0));
    assert_eq!(stats.execs, seen.len());
}

/// COMPLETENESS / OPTIMALITY. A symbolic evaluation decides whether a thread is
/// spawned at all.
///
/// Derived: x>0 true => the sender exists, and `main`'s non-blocking receive
/// reads nothing or reads the send => 2 graphs. x>0 false => no sender, the
/// receive reads nothing => 1 graph. Total 3 graphs, 0 blocked.
#[test]
fn condpor_i3_evaluation_decides_whether_a_thread_is_spawned() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let main_id = thread::current_id();
        let x = symbolic::fresh_int();

        let positive = symbolic::eval(x.gt(0));
        let handle = if positive {
            Some(thread::spawn(move || {
                send_msg(main_id, 1_i32);
            }))
        } else {
            None
        };

        let got = recv_msg::<i32>().is_some();
        if let Some(h) = handle {
            h.join().unwrap();
        }
        observed_in_verify.lock().unwrap().insert((positive, got));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        !seen.contains(&(false, true)),
        "SOUNDNESS VIOLATION: a message was received from a thread that was never spawned: {seen:?}"
    );
    assert_eq!(
        seen,
        HashSet::from([(true, true), (true, false), (false, false)])
    );
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
}

/// SOUNDNESS. `assume` prunes a branch that a later backward revisit would
/// otherwise reach: the sender only reaches its send on the `x <= 0` branch.
///
/// Derived:
/// - first execution: the deleted thread takes `x>0` true, the sender's
///   `assume(x<=0)` is then forced false and the execution blocks => 1 blocked.
/// - forward revisit: `x>0` false, the assume passes, the sender sends, and
///   `main`'s receive reads nothing => 1 graph.
/// - backward revisit: the receive reads the send; the deleted `x>0` must be
///   re-decided as false under the retained `x<=0` => 1 graph.
///
/// Total 2 graphs, 1 blocked; `x>0` is false in every complete graph.
#[test]
fn condpor_i5_assume_prunes_a_branch_a_revisit_would_reach() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let main_id = thread::current_id();
        let x = symbolic::fresh_int();

        let del = Arc::new(Mutex::new(None));
        let del_in = del.clone();
        let xd = x.clone();
        let t_del = thread::spawn(move || {
            *del_in.lock().unwrap() = Some(symbolic::eval(xd.gt(0)));
        });

        let t_ret = thread::spawn(move || {
            symbolic::assume(x.le(0));
            send_msg(main_id, 1_i32);
        });

        let got = recv_msg::<i32>().is_some();
        t_del.join().unwrap();
        t_ret.join().unwrap();

        let d = (*del.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert((d, got));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        !seen.iter().any(|(d, _)| *d),
        "SOUNDNESS VIOLATION: x > 0 held in a graph whose assume requires x <= 0: {seen:?}"
    );
    assert_eq!(seen, HashSet::from([(false, false), (false, true)]));
    assert_eq!((stats.execs, stats.block), (2, 1));
    assert_eq!(stats.execs, seen.len());
}

/// The same symbolic variable evaluated by three threads, with monotone
/// thresholds so the feasible set is strictly smaller than the cube.
///
/// Derived feasible assignments of (x>0, x>10, x>100): (F,F,F), (T,F,F),
/// (T,T,F), (T,T,T). 4 graphs, 0 blocked.
#[test]
fn condpor_i6_one_variable_three_threads() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();
        let obs = Arc::new(Mutex::new([None, None, None]));

        let o1 = obs.clone();
        let x1 = x.clone();
        let t1 = thread::spawn(move || {
            o1.lock().unwrap()[0] = Some(symbolic::eval(x1.gt(0)));
        });
        let o2 = obs.clone();
        let x2 = x.clone();
        let t2 = thread::spawn(move || {
            o2.lock().unwrap()[1] = Some(symbolic::eval(x2.gt(10)));
        });
        let o3 = obs.clone();
        let t3 = thread::spawn(move || {
            o3.lock().unwrap()[2] = Some(symbolic::eval(x.gt(100)));
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        let v = *obs.lock().unwrap();
        observed_in_verify
            .lock()
            .unwrap()
            .insert((v[0].unwrap(), v[1].unwrap(), v[2].unwrap()));
    });

    let seen = observed.lock().unwrap().clone();
    assert!(
        seen.iter().all(|(a, b, c)| (!*b || *a) && (!*c || *b)),
        "SOUNDNESS VIOLATION: a non-monotone threshold assignment: {seen:?}"
    );
    assert_eq!(
        seen,
        HashSet::from([
            (false, false, false),
            (true, false, false),
            (true, true, false),
            (true, true, true),
        ])
    );
    assert_eq!((stats.execs, stats.block), (4, 0));
    assert_eq!(stats.execs, seen.len());
}

/// Blocked executions with symbolic constraints present.
/// `eval(x>0)` then `assume(x<0)`.
///
/// Derived: (x>0 true) => `x<0` forced false => blocked. (x>0 false) =>
/// `x<0` is free: true passes the assume (1 graph), false (x == 0) blocks.
/// Total 1 graph, 2 blocked.
#[test]
fn condpor_i7_blocked_executions_with_constraints() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let x = symbolic::fresh_int();
        let positive = symbolic::eval(x.clone().gt(0));
        symbolic::assume(x.lt(0));
        observed_in_verify.lock().unwrap().insert(positive);
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(seen, HashSet::from([false]));
    assert_eq!((stats.execs, stats.block), (1, 2));
}

// ---------------------------------------------------------------------------
// Item 3 (continued) — a symbolic evaluation inside a monitor
// ---------------------------------------------------------------------------

/// A monitor is an ordinary TraceForge thread that `recv_msg_block`s in a loop
/// and calls `notify` (see `traceforge-macros`' generated `start_monitor_*`),
/// so `notify` runs in a thread context where `symbolic::eval` is callable.
/// This records whether the symbolic machinery survives being driven from a
/// monitor's observer.
#[monitor(i32)]
#[derive(Clone, Debug, Default)]
pub struct SymbolicGate {}

impl Monitor for SymbolicGate {}

impl Acceptor<i32> for SymbolicGate {
    fn accept(&mut self, _who: thread::ThreadId, _whom: thread::ThreadId, _what: &i32) -> bool {
        true
    }
}

impl Observer<i32> for SymbolicGate {
    fn notify(
        &mut self,
        _who: thread::ThreadId,
        _whom: thread::ThreadId,
        _what: &i32,
    ) -> MonitorResult {
        let x = symbolic::fresh_int();
        let positive = symbolic::eval(x.gt(0));
        MONITOR_BRANCHES.lock().unwrap().push(positive);
        Ok(())
    }
}

static MONITOR_BRANCHES: Mutex<Vec<bool>> = Mutex::new(Vec::new());

/// Derived: the monitor's evaluation is the only branching point, so it must
/// produce exactly two complete executions and no blocked ones, and both
/// outcomes must be observed.
#[test]
fn condpor_i4_symbolic_evaluation_inside_a_monitor() {
    MONITOR_BRANCHES.lock().unwrap().clear();

    let stats = verify(ltr(), || {
        let mon = start_monitor_symbolic_gate(SymbolicGate {});

        let worker = thread::spawn(|| {
            let _v: i32 = recv_msg_block();
        });
        send_msg(worker.thread().id(), 7_i32);
        worker.join().unwrap();

        terminate_monitor_symbolic_gate(mon.thread().id());
        let _ = mon.join().unwrap();
    });

    let recorded = MONITOR_BRANCHES.lock().unwrap().clone();
    let seen: HashSet<bool> = recorded.iter().cloned().collect();
    assert_eq!(seen, HashSet::from([true, false]), "{recorded:?}");
    assert_eq!((stats.execs, stats.block), (2, 0));
}

/// COMPLETENESS / SOUNDNESS. The constraint event that references a received
/// symbolic value is created *by* a backward revisit: the evaluator's
/// non-blocking receive reads nothing first, and only the generator's send
/// backward-revisits it, after which the evaluator's constraint over the
/// received variable is a newly added event whose generator is a retained one.
/// This is the direct attack on "a retained constraint referencing a generator
/// a revisit deleted": here the generator sits in the send's porf-prefix, so it
/// must survive, and the new constraint must be decidable.
///
/// Derived: the receive reads nothing (1 graph, no constraint at all), or reads
/// the generator's send and then evaluates `x > 0` either way (2 graphs).
/// Total 3 graphs, 0 blocked.
#[test]
fn condpor_f6b_constraint_created_by_a_backward_revisit_over_a_received_variable() {
    let observed = Arc::new(Mutex::new(HashSet::new()));
    let observed_in_verify = observed.clone();

    let stats = verify(ltr(), move || {
        let obs = Arc::new(Mutex::new(None));
        let obs_in = obs.clone();

        let evaluator = thread::spawn(move || {
            let v: Option<symbolic::SymExpr> = recv_msg();
            let branch = v.map(|v| symbolic::eval(v.gt(0)));
            *obs_in.lock().unwrap() = Some(branch);
        });
        let ev_id = evaluator.thread().id();

        let generator = thread::spawn(move || {
            let x = symbolic::fresh_int();
            send_msg(ev_id, x);
        });

        generator.join().unwrap();
        evaluator.join().unwrap();

        let branch = (*obs.lock().unwrap()).unwrap();
        observed_in_verify.lock().unwrap().insert(branch);
    });

    let seen = observed.lock().unwrap().clone();
    assert_eq!(seen, HashSet::from([None, Some(true), Some(false)]));
    assert_eq!((stats.execs, stats.block), (3, 0));
    assert_eq!(stats.execs, seen.len());
}
