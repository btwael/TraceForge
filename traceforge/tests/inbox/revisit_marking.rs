//! A send selected by an inbox revisit is not anchored if it precedes that inbox.

use std::sync::{Arc, Mutex};

use traceforge::{thread, Config, ConsType, SchedulePolicy};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Msg {
    A,
    Z,
    B,
    T,
    SA,
    SZ,
    W,
}

fn collect(count: usize) -> Vec<Msg> {
    let mut batch: Vec<_> = traceforge::inbox_with_bounds(count, Some(count))
        .into_iter()
        .flatten()
        .map(|value| *value.as_any_ref().downcast_ref::<Msg>().unwrap())
        .collect();
    batch.sort();
    batch
}

fn check_old_selected_send(model: ConsType, use_collect: bool) {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&outcomes);
    let stats = traceforge::verify(
        Config::builder()
            .with_cons_type(model)
            .with_policy(SchedulePolicy::LTR)
            .build(),
        move || {
            let main_id = thread::main_thread_id();
            let receiver = thread::spawn(|| collect(2));
            let receiver_id = receiver.thread().id();
            let delayed = thread::spawn(move || {
                assert_eq!(collect(1), vec![Msg::W]);
                traceforge::send_msg(receiver_id, Msg::T);
                traceforge::send_msg(main_id, Msg::Z);
            });
            let delayed_id = delayed.thread().id();
            let independent = thread::spawn(move || traceforge::send_msg(receiver_id, Msg::B));
            let wakeup = thread::spawn(move || traceforge::send_msg(delayed_id, Msg::W));

            traceforge::send_msg(main_id, Msg::A);
            let choice = if use_collect {
                collect(1)[0]
            } else {
                traceforge::recv_msg_block::<Msg>()
            };
            traceforge::send_msg(
                receiver_id,
                if choice == Msg::A { Msg::SA } else { Msg::SZ },
            );
            let batch = receiver.join().unwrap();
            recorded.lock().unwrap().push((choice, batch));
            delayed.join().unwrap();
            independent.join().unwrap();
            wakeup.join().unwrap();
        },
    );

    // Revisiting {SA,B} to {SA,T} must not freeze the earlier receive of A:
    // Z can revisit it, deleting SA and the downstream inbox before re-execution.
    let mut expected = vec![
        (Msg::A, vec![Msg::B, Msg::T]),
        (Msg::A, vec![Msg::B, Msg::SA]),
        (Msg::A, vec![Msg::T, Msg::SA]),
        (Msg::Z, vec![Msg::B, Msg::T]),
        (Msg::Z, vec![Msg::T, Msg::SZ]),
    ];
    if matches!(model, ConsType::Bag | ConsType::FIFO) {
        // Causal and Mailbox require T before SZ, through T -> Z -> SZ.
        expected.push((Msg::Z, vec![Msg::B, Msg::SZ]));
    }
    expected.sort();
    let mut actual = outcomes.lock().unwrap().clone();
    actual.sort();
    assert_eq!(stats.block, 0, "{model:?}, use_collect={use_collect}");
    assert_eq!(
        stats.execs,
        expected.len(),
        "{model:?}, use_collect={use_collect}"
    );
    assert_eq!(actual, expected, "{model:?}, use_collect={use_collect}");
    actual.dedup();
    assert_eq!(
        actual.len(),
        stats.execs,
        "duplicate graph: {model:?}, use_collect={use_collect}"
    );
}

#[test]
fn old_selected_send_does_not_freeze_preceding_inbox() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        check_old_selected_send(model, true);
    }
}

#[test]
fn old_selected_send_does_not_freeze_preceding_recv() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        check_old_selected_send(model, false);
    }
}
