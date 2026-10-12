use std::sync::{Arc, Mutex};

use traceforge::{thread, Config, ConsType, SchedulePolicy, Stats};

const DATA: u32 = 1;
const READY: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Msg {
    A,
    Z,
    T,
    SA,
    SZ,
    Ready,
}

fn collect(n: usize) -> Vec<Msg> {
    traceforge::inbox_with_tag_and_bounds(|_, tag| tag == Some(DATA), n, Some(n))
        .into_iter()
        .flatten()
        .map(|value| *value.as_any_ref().downcast_ref::<Msg>().unwrap())
        .collect()
}

fn explore(atomic: bool, model: ConsType) -> (Stats, Vec<(Msg, Vec<Msg>)>) {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&outcomes);
    let stats = traceforge::verify(
        Config::builder()
            .with_cons_type(model)
            .with_policy(SchedulePolicy::LTR)
            .build(),
        move || {
            let p0 = thread::main_thread_id();
            let p1 = thread::spawn(move || {
                let ready: Msg = traceforge::recv_tagged_msg_block(|_, tag| tag == Some(READY));
                assert_eq!(ready, Msg::Ready);
                if atomic {
                    let mut batch = collect(2);
                    batch.sort();
                    batch
                } else {
                    vec![collect(1)[0], collect(1)[0]]
                }
            });
            let p1_id = p1.thread().id();
            let p2 = thread::spawn(move || {
                traceforge::send_tagged_msg(p1_id, DATA, Msg::T);
                traceforge::send_tagged_msg(p0, DATA, Msg::Z);
            });
            let fence = thread::spawn(|| {});

            traceforge::send_tagged_msg(p0, DATA, Msg::A);
            // Both choices are available before y, and both P1 messages before
            // its collects: this exercises forward consistency, not revisits.
            // An independent later thread runs after P2 under LTR, without
            // making T causally precede S(A) as joining P2 here would do.
            fence.join().unwrap();
            let y = collect(1)[0];
            traceforge::send_tagged_msg(p1_id, DATA, if y == Msg::A { Msg::SA } else { Msg::SZ });
            traceforge::send_tagged_msg(p1_id, READY, Msg::Ready);
            let values = p1.join().unwrap();
            recorded.lock().unwrap().push((y, values));
            p2.join().unwrap();
        },
    );
    let mut actual = outcomes.lock().unwrap().clone();
    actual.sort();
    (stats, actual)
}

#[test]
fn one_atomic_batch_does_not_induce_mailbox_edges_between_its_sends() {
    let expected = vec![
        (Msg::A, vec![Msg::T, Msg::SA]),
        (Msg::Z, vec![Msg::T, Msg::SZ]),
    ];
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let (stats, actual) = explore(true, model);
        // If y=Z, a spurious S(Z)->T batch edge would create A->S(Z)->T->Z
        // and incorrectly forbid leaving A unread. T<S(Z) is a valid mailbox order.
        assert_eq!(stats.block, 0, "{model:?}");
        assert_eq!(stats.execs, expected.len(), "{model:?}");
        assert_eq!(actual, expected, "{model:?}");
    }
}

#[test]
fn distinct_collects_still_induce_mailbox_order() {
    for model in [
        ConsType::Bag,
        ConsType::FIFO,
        ConsType::Causal,
        ConsType::Mailbox,
    ] {
        let mut expected = vec![
            (Msg::A, vec![Msg::T, Msg::SA]),
            (Msg::A, vec![Msg::SA, Msg::T]),
            (Msg::Z, vec![Msg::T, Msg::SZ]),
        ];
        if matches!(model, ConsType::Bag | ConsType::FIFO) {
            expected.push((Msg::Z, vec![Msg::SZ, Msg::T]));
        }
        expected.sort();
        let (stats, actual) = explore(false, model);
        // When y=Z, T is causally before S(Z); unlike one atomic batch,
        // two separate collects must not read S(Z) before T under Mailbox.
        assert_eq!(stats.block, 0, "{model:?}");
        assert_eq!(stats.execs, expected.len(), "{model:?}");
        assert_eq!(actual, expected, "{model:?}");
    }
}
