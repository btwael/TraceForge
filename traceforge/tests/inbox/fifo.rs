use std::sync::{Arc, Mutex};

use traceforge::{self, thread, Config};

fn read_u32_inbox() -> Vec<u32> {
    let mut batch: Vec<u32> = traceforge::inbox()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_any_ref().downcast_ref::<u32>().copied())
        .collect();
    batch.sort_unstable();
    batch
}

fn sorted_outcomes(outcomes: &Arc<Mutex<Vec<Vec<u32>>>>) -> Vec<Vec<u32>> {
    let mut outcomes = outcomes.lock().unwrap().clone();
    outcomes.sort();
    outcomes
}

#[test]
fn fifo_inbox_forward_reads_only_prefixes() {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&outcomes);

    let stats = traceforge::verify(Config::builder().build(), move || {
        let target = thread::main_thread_id();
        let sender = thread::spawn(move || {
            traceforge::send_msg(target, 1u32);
            traceforge::send_msg(target, 2u32);
        });

        // Both sends are in the graph before the inbox is created, so all
        // alternatives are forward branches.
        let _ = sender.join();
        recorded.lock().unwrap().push(read_u32_inbox());
    });

    assert_eq!(stats.execs, 3);
    assert_eq!(stats.block, 0);
    assert_eq!(
        sorted_outcomes(&outcomes),
        vec![vec![], vec![1], vec![1, 2]]
    );
}

#[test]
fn fifo_inbox_backward_revisit_reads_only_prefixes() {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&outcomes);

    let stats = traceforge::verify(Config::builder().build(), move || {
        let target = thread::main_thread_id();
        let sender = thread::spawn(move || {
            traceforge::send_msg(target, 1u32);
            traceforge::send_msg(target, 2u32);
        });

        // Under LTR the main-thread inbox is initially created before the
        // sender runs; the non-empty alternatives are found by backward revisit.
        recorded.lock().unwrap().push(read_u32_inbox());
        let _ = sender.join();
    });

    assert_eq!(stats.execs, 3);
    assert_eq!(stats.block, 0);
    assert_eq!(
        sorted_outcomes(&outcomes),
        vec![vec![], vec![1], vec![1, 2]]
    );
}

#[test]
fn fifo_inbox_can_skip_an_explicitly_dropped_predecessor() {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&outcomes);

    let stats = traceforge::verify(Config::builder().with_lossy(1).build(), move || {
        let target = thread::main_thread_id();
        let sender = thread::spawn(move || {
            traceforge::send_lossy_msg(target, 1u32);
            traceforge::send_msg(target, 2u32);
        });

        let _ = sender.join();
        recorded.lock().unwrap().push(read_u32_inbox());
    });

    let outcomes = sorted_outcomes(&outcomes);
    assert_eq!(stats.block, 0);
    assert!(outcomes.contains(&vec![2]));
    assert!(!outcomes.is_empty());
}
