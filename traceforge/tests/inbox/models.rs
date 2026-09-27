use std::sync::{Arc, Mutex};

use traceforge::{self, thread, Config, ConsType, SchedulePolicy, Stats};

fn batch_with_bounds(min: usize, max: Option<usize>) -> Vec<u32> {
    let mut values: Vec<u32> = traceforge::inbox_with_bounds(min, max)
        .into_iter()
        .flatten()
        .map(|value| *value.as_any_ref().downcast_ref::<u32>().unwrap())
        .collect();
    values.sort_unstable();
    values
}

fn batch() -> Vec<u32> {
    batch_with_bounds(0, None)
}

fn optional_singleton_batch() -> Option<u32> {
    traceforge::inbox_with_bounds(0, Some(1))
        .into_iter()
        .flatten()
        .next()
        .map(|value| *value.as_any_ref().downcast_ref::<u32>().unwrap())
}

fn mixed_pair(inbox_first: bool) -> (Option<u32>, Vec<u32>) {
    if inbox_first {
        let values = batch();
        let value: Option<u32> = traceforge::recv_msg();
        (value, values)
    } else {
        let value: Option<u32> = traceforge::recv_msg();
        let values = batch();
        (value, values)
    }
}

fn ordered_two(model: ConsType, backward: bool, policy: SchedulePolicy) -> (Stats, Vec<Vec<u32>>) {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&outcomes);
    let stats = traceforge::verify(
        Config::builder()
            .with_cons_type(model)
            .with_policy(policy)
            .build(),
        move || {
            let target = thread::main_thread_id();
            let sender = thread::spawn(move || {
                traceforge::send_msg(target, 1u32);
                traceforge::send_msg(target, 2u32);
            });
            if backward {
                recorded.lock().unwrap().push(batch());
                let _ = sender.join();
            } else {
                let _ = sender.join();
                recorded.lock().unwrap().push(batch());
            }
        },
    );
    let mut values = outcomes.lock().unwrap().clone();
    values.sort();
    (stats, values)
}

#[test]
fn one_inbox_two_ordered_sends_forward_and_backward() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let mut expected = vec![vec![], vec![1], vec![1, 2]];
        if model == ConsType::Bag {
            expected.push(vec![2]);
        }
        expected.sort();
        for backward in [false, true] {
            let (stats, actual) = ordered_two(model, backward, SchedulePolicy::LTR);
            assert_eq!(stats.block, 0, "{model:?}, backward={backward}");
            assert_eq!(
                stats.execs,
                expected.len(),
                "{model:?}, backward={backward}"
            );
            assert_eq!(actual, expected, "{model:?}, backward={backward}");
        }
    }
}

#[test]
fn arbitrary_schedule_preserves_each_ordered_batch_once() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let mut expected = vec![vec![], vec![1], vec![1, 2]];
        if model == ConsType::Bag {
            expected.push(vec![2]);
        }
        expected.sort();
        for _ in 0..5 {
            let (stats, actual) = ordered_two(model, true, SchedulePolicy::Arbitrary);
            assert_eq!(stats.block, 0, "{model:?}");
            assert_eq!(stats.execs, expected.len(), "{model:?}");
            assert_eq!(actual, expected, "{model:?}");
        }
    }
}

#[test]
fn two_inboxes_ordered_sends_have_each_delivery_once() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let mut expected = vec![
            (vec![], vec![]),
            (vec![], vec![1]),
            (vec![], vec![1, 2]),
            (vec![1], vec![]),
            (vec![1], vec![2]),
            (vec![1, 2], vec![]),
        ];
        if model == ConsType::Bag {
            expected.extend([(vec![], vec![2]), (vec![2], vec![]), (vec![2], vec![1])]);
        }
        expected.sort();
        for backward in [false, true] {
            let outcomes = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&outcomes);
            let stats =
                traceforge::verify(Config::builder().with_cons_type(model).build(), move || {
                    let target = thread::main_thread_id();
                    let sender = thread::spawn(move || {
                        traceforge::send_msg(target, 1u32);
                        traceforge::send_msg(target, 2u32);
                    });
                    if backward {
                        let first = batch();
                        let second = batch();
                        recorded.lock().unwrap().push((first, second));
                        let _ = sender.join();
                    } else {
                        let _ = sender.join();
                        let first = batch();
                        let second = batch();
                        recorded.lock().unwrap().push((first, second));
                    }
                });
            let mut actual = outcomes.lock().unwrap().clone();
            actual.sort();
            assert_eq!(stats.block, 0, "{model:?}, backward={backward}");
            assert_eq!(
                stats.execs,
                expected.len(),
                "{model:?}, backward={backward}"
            );
            assert_eq!(actual, expected, "{model:?}, backward={backward}");
        }
    }
}

#[test]
fn independent_senders_exercise_backward_subset_maximality() {
    let expected: Vec<Vec<u32>> = (0..8)
        .map(|mask| {
            (0..3)
                .filter(|bit| mask & (1 << bit) != 0)
                .map(|bit| bit + 1)
                .collect()
        })
        .collect();
    let mut expected = expected;
    expected.sort();
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        for backward in [false, true] {
            let outcomes = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&outcomes);
            let stats =
                traceforge::verify(Config::builder().with_cons_type(model).build(), move || {
                    let target = thread::main_thread_id();
                    let senders: Vec<_> = (1u32..=3)
                        .map(|value| {
                            let id = target.clone();
                            thread::spawn(move || traceforge::send_msg(id, value))
                        })
                        .collect();
                    if !backward {
                        for sender in senders {
                            let _ = sender.join();
                        }
                        recorded.lock().unwrap().push(batch());
                    } else {
                        recorded.lock().unwrap().push(batch());
                        for sender in senders {
                            let _ = sender.join();
                        }
                    }
                });
            let mut actual = outcomes.lock().unwrap().clone();
            actual.sort();
            assert_eq!(stats.block, 0, "{model:?}, backward={backward}");
            assert_eq!(stats.execs, 8, "{model:?}, backward={backward}");
            assert_eq!(actual, expected, "{model:?}, backward={backward}");
        }
    }
}

#[test]
fn causal_predecessor_from_another_sender_is_required() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        for min in [0, 1, 2] {
            let outcomes = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&outcomes);
            let stats =
                traceforge::verify(Config::builder().with_cons_type(model).build(), move || {
                    let target = thread::main_thread_id();
                    let later_target = target.clone();
                    // This thread has the earlier Event ordering but sends 2 only
                    // after receiving a signal from the thread that sends 1.
                    let later = thread::spawn(move || {
                        let _: u8 = traceforge::recv_msg_block();
                        traceforge::send_msg(later_target, 2u32);
                    });
                    let later_id = later.thread().id();
                    let earlier = thread::spawn(move || {
                        traceforge::send_msg(target, 1u32);
                        traceforge::send_msg(later_id, 7u8);
                    });
                    let _ = earlier.join();
                    let _ = later.join();
                    recorded.lock().unwrap().push(batch_with_bounds(min, None));
                });
            let mut actual = outcomes.lock().unwrap().clone();
            actual.sort();
            let mut expected = vec![vec![1, 2]];
            if min <= 1 {
                expected.push(vec![1]);
            }
            if min == 0 {
                expected.push(vec![]);
            }
            if min <= 1 && matches!(model, ConsType::Bag | ConsType::FIFO) {
                expected.push(vec![2]);
            }
            expected.sort();
            assert_eq!(stats.block, 0, "{model:?}, min={min}");
            assert_eq!(stats.execs, expected.len(), "{model:?}, min={min}");
            assert_eq!(actual, expected, "{model:?}, min={min}");
        }
    }
}

fn lower_id_sender_wakes_late(model: ConsType, max: usize) -> (Stats, Vec<Vec<u32>>) {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&outcomes);
    let stats = traceforge::verify(
        Config::builder()
            .with_cons_type(model)
            .with_policy(SchedulePolicy::LTR)
            .build(),
        move || {
            let target = thread::main_thread_id();
            let late_target = target;
            let late = thread::spawn(move || {
                let _: u8 = traceforge::recv_msg_block();
                traceforge::send_msg(late_target, 1u32);
            });
            let late_id = late.thread().id();
            let early = thread::spawn(move || {
                traceforge::send_msg(target, 2u32);
                traceforge::send_msg(late_id, 7u8);
            });

            // Main blocks here. The send of 2 wakes it before the lower-ID
            // thread receives the signal and sends 1, despite LTR scheduling.
            recorded
                .lock()
                .unwrap()
                .push(batch_with_bounds(1, Some(max)));
            let _ = late.join();
            let _ = early.join();
        },
    );
    let mut actual = outcomes.lock().unwrap().clone();
    actual.sort();
    (stats, actual)
}

#[test]
fn ltr_lower_id_sender_wakes_late_singleton_inbox() {
    let mut failures = Vec::new();
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let (stats, actual) = lower_id_sender_wakes_late(model, 1);
        let mut expected = vec![vec![2]];
        if matches!(model, ConsType::Bag | ConsType::FIFO) {
            expected.push(vec![1]);
        }
        expected.sort();
        if stats.block != 0 || stats.execs != expected.len() || actual != expected {
            failures.push(format!(
                "{model:?}: expected {expected:?} ({} executions, 0 blocked), got {actual:?} ({} executions, {} blocked)",
                expected.len(), stats.execs, stats.block
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn ltr_lower_id_sender_wakes_late_multi_message_inbox() {
    let mut failures = Vec::new();
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let (stats, actual) = lower_id_sender_wakes_late(model, 2);
        let mut expected = vec![vec![2], vec![1, 2]];
        if matches!(model, ConsType::Bag | ConsType::FIFO) {
            expected.push(vec![1]);
        }
        expected.sort();
        if stats.block != 0 || stats.execs != expected.len() || actual != expected {
            failures.push(format!(
                "{model:?}: expected {expected:?} ({} executions, 0 blocked), got {actual:?} ({} executions, {} blocked)",
                expected.len(), stats.execs, stats.block
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn ltr_nonmonotone_three_sends_visit_each_batch_once() {
    let expected = vec![
        vec![1],
        vec![1, 2],
        vec![1, 2, 3],
        vec![1, 3],
        vec![2],
        vec![2, 3],
        vec![3],
    ];
    for model in [ConsType::Bag, ConsType::FIFO] {
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&outcomes);
        let stats = traceforge::verify(
            Config::builder()
                .with_cons_type(model)
                .with_policy(SchedulePolicy::LTR)
                .build(),
            move || {
                let target = thread::main_thread_id();
                let late_target = target;
                let late = thread::spawn(move || {
                    let _: u8 = traceforge::recv_msg_block();
                    traceforge::send_msg(late_target, 1u32);
                });
                let late_id = late.thread().id();
                let early = thread::spawn(move || {
                    traceforge::send_msg(target, 2u32);
                    traceforge::send_msg(late_id, 7u8);
                });
                let third = thread::spawn(move || traceforge::send_msg(target, 3u32));

                recorded.lock().unwrap().push(batch_with_bounds(1, None));
                let _ = late.join();
                let _ = early.join();
                let _ = third.join();
            },
        );
        let mut actual = outcomes.lock().unwrap().clone();
        actual.sort();
        assert_eq!(stats.block, 0, "{model:?}");
        assert_eq!(stats.execs, expected.len(), "{model:?}");
        assert_eq!(actual, expected, "{model:?}");
    }
}

#[test]
fn recv_and_inbox_share_delivery_order_in_both_directions() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        for inbox_first in [false, true] {
            for backward in [false, true] {
                let outcomes = Arc::new(Mutex::new(Vec::new()));
                let recorded = Arc::clone(&outcomes);
                let stats = traceforge::verify(
                    Config::builder().with_cons_type(model).build(),
                    move || {
                        let target = thread::main_thread_id();
                        let sender = thread::spawn(move || {
                            traceforge::send_msg(target, 1u32);
                            traceforge::send_msg(target, 2u32);
                        });
                        if backward {
                            recorded.lock().unwrap().push(mixed_pair(inbox_first));
                            let _ = sender.join();
                        } else {
                            let _ = sender.join();
                            recorded.lock().unwrap().push(mixed_pair(inbox_first));
                        }
                    },
                );
                let mut actual = outcomes.lock().unwrap().clone();
                actual.sort();
                let mut expected = if inbox_first {
                    vec![
                        (None, vec![]),
                        (Some(1), vec![]),
                        (None, vec![1]),
                        (Some(2), vec![1]),
                        (None, vec![1, 2]),
                    ]
                } else {
                    vec![
                        (None, vec![]),
                        (None, vec![1]),
                        (None, vec![1, 2]),
                        (Some(1), vec![]),
                        (Some(1), vec![2]),
                    ]
                };
                if model == ConsType::Bag {
                    if inbox_first {
                        expected.extend([(None, vec![2]), (Some(1), vec![2]), (Some(2), vec![])]);
                    } else {
                        expected.extend([(None, vec![2]), (Some(2), vec![]), (Some(2), vec![1])]);
                    }
                }
                expected.sort();
                assert_eq!(
                    stats.block, 0,
                    "{model:?}, inbox_first={inbox_first}, backward={backward}"
                );
                assert_eq!(
                    stats.execs,
                    expected.len(),
                    "{model:?}, inbox_first={inbox_first}, backward={backward}"
                );
                assert_eq!(
                    actual, expected,
                    "{model:?}, inbox_first={inbox_first}, backward={backward}"
                );
            }
        }
    }
}

#[test]
fn mailbox_global_order_with_mixed_receivers() {
    for model in [ConsType::Causal, ConsType::Mailbox] {
        for require_cycle in [false, true] {
            let stats =
                traceforge::verify(Config::builder().with_cons_type(model).build(), move || {
                    let x = thread::main_thread_id();
                    let y = thread::spawn(move || {
                        let first: Option<u32> = traceforge::recv_msg();
                        let second = optional_singleton_batch();
                        (first, second)
                    });
                    let y_id = y.thread().id();
                    let x_a = x.clone();
                    let y_a = y_id.clone();
                    let a = thread::spawn(move || {
                        traceforge::send_msg(x_a, 1u32);
                        traceforge::send_msg(y_a, 3u32);
                    });
                    let b = thread::spawn(move || {
                        traceforge::send_msg(y_id, 4u32);
                        traceforge::send_msg(x, 2u32);
                    });
                    let _ = a.join();
                    let _ = b.join();
                    let x_first = optional_singleton_batch();
                    let x_second: Option<u32> = traceforge::recv_msg();
                    let (y_first, y_second) = y.join().unwrap();
                    let complete = x_first.is_some()
                        && x_second.is_some()
                        && y_first.is_some()
                        && y_second.is_some();
                    let cycle = (x_first, x_second, y_first, y_second)
                        == (Some(2), Some(1), Some(3), Some(4));
                    traceforge::assume!(complete && (!require_cycle || cycle));
                });
            // Failed assumptions are reported as blocked executions; only
            // completed graphs satisfying the litmus predicate are counted.
            let expected = if require_cycle {
                usize::from(model == ConsType::Causal)
            } else if model == ConsType::Causal {
                4
            } else {
                3
            };
            assert_eq!(stats.execs, expected, "{model:?}, cycle={require_cycle}");
        }
    }
}

#[test]
fn explicit_loss_can_expose_a_later_message_without_implicit_loss() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&outcomes);
        let stats = traceforge::verify(
            Config::builder()
                .with_cons_type(model)
                .with_lossy(1)
                .build(),
            move || {
                let target = thread::main_thread_id();
                let sender = thread::spawn(move || {
                    traceforge::send_lossy_msg(target, 1u32);
                    traceforge::send_msg(target, 2u32);
                });
                let _ = sender.join();
                recorded.lock().unwrap().push(batch());
            },
        );
        let mut actual = outcomes.lock().unwrap().clone();
        actual.sort();
        let mut expected = vec![vec![], vec![], vec![1], vec![1, 2], vec![2]];
        if model == ConsType::Bag {
            expected.push(vec![2]);
        }
        expected.sort();
        assert_eq!(stats.block, 0, "{model:?}");
        assert_eq!(stats.execs, expected.len(), "{model:?}");
        assert_eq!(actual, expected, "{model:?}");
    }
}

#[test]
fn tagged_inbox_skips_only_nonmatching_predecessors_then_recv_can_take_them() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&outcomes);
        let stats =
            traceforge::verify(Config::builder().with_cons_type(model).build(), move || {
                let target = thread::main_thread_id();
                let sender = thread::spawn(move || {
                    traceforge::send_tagged_msg(target.clone(), 1, 1u32);
                    traceforge::send_tagged_msg(target, 2, 2u32);
                });
                let _ = sender.join();
                let selected = traceforge::inbox_with_tag(|_, tag| tag == Some(2))
                    .into_iter()
                    .flatten()
                    .next()
                    .map(|value| *value.as_any_ref().downcast_ref::<u32>().unwrap());
                let remaining: Option<u32> = traceforge::recv_msg();
                recorded.lock().unwrap().push((selected, remaining));
            });
        let mut actual = outcomes.lock().unwrap().clone();
        actual.sort();
        let mut expected = vec![
            (None, None),
            (None, Some(1)),
            (Some(2), None),
            (Some(2), Some(1)),
        ];
        if model == ConsType::Bag {
            expected.push((None, Some(2)));
        }
        expected.sort();
        assert_eq!(stats.block, 0, "{model:?}");
        assert_eq!(stats.execs, expected.len(), "{model:?}");
        assert_eq!(actual, expected, "{model:?}");
    }
}
