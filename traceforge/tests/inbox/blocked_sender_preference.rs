//! A lower-ID sender can send after a higher-ID sender when it first blocks.
//! This is the P1/P2/P3 singleton-collect counterexample; all sends share one round.

use std::sync::{Arc, Mutex};

use traceforge::{thread, Config, ConsType, SchedulePolicy};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Msg {
    A,
    B,
    C,
}

fn receive_one(use_collect: bool) -> Msg {
    if use_collect {
        let mut batch = traceforge::inbox_with_bounds(1, Some(1));
        let value = batch
            .pop()
            .flatten()
            .expect("singleton collect must receive");
        *value.as_any_ref().downcast_ref::<Msg>().unwrap()
    } else {
        traceforge::recv_msg_block::<Msg>()
    }
}

fn explore(model: ConsType, use_collect: bool) -> (usize, usize, Vec<Msg>) {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&outcomes);
    let stats = traceforge::verify(
        Config::builder()
            .with_cons_type(model)
            .with_policy(SchedulePolicy::LTR)
            .build(),
        move || {
            let p1 = thread::main_thread_id();
            let p2_target = p1.clone();
            let p2 = thread::spawn(move || {
                let y = receive_one(use_collect);
                assert_eq!(y, Msg::C);
                traceforge::send_msg(p2_target, Msg::B);
            });
            let p2_id = p2.thread().id();
            let p3 = thread::spawn(move || {
                traceforge::send_msg(p1, Msg::A);
                traceforge::send_msg(p2_id, Msg::C);
            });

            let x = receive_one(use_collect);
            recorded.lock().unwrap().push(x);
            p2.join().unwrap();
            p3.join().unwrap();
        },
    );
    let mut outcomes = outcomes.lock().unwrap().clone();
    outcomes.sort();
    (stats.execs, stats.block, outcomes)
}

#[test]
fn blocking_lower_id_sender_matches_singleton_recv_baseline() {
    for model in [ConsType::Bag, ConsType::FIFO] {
        let baseline = explore(model, false);
        let collect = explore(model, true);
        assert_eq!(baseline, (2, 0, vec![Msg::A, Msg::B]), "{model:?}");
        assert_eq!(collect, baseline, "{model:?}: collect missed a graph");
    }
}
