use traceforge::comm_close::{self, Rounds};
use traceforge::thread;

fn example(senders: u32) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let main_id = thread::main_thread_id();
        let rounds = Rounds::new();

        let mut handles = Vec::new();
        for i in 0..senders {
            let main_id = main_id;
            handles.push(thread::spawn(move || {
                let rounds = Rounds::new();
                let round = rounds.current();
                comm_close::send(main_id, i, &round);
            }));
        }

        let round = rounds.current();
        let messages =
            comm_close::inbox_with_bounds(&round, 1, Some(senders as usize));
        assert!(!messages.is_empty());

        for msg in messages.into_iter().flatten() {
            let val = msg
                .payload(&round)
                .as_any_ref()
                .downcast_ref::<u32>()
                .expect("expected a u32 payload");
            assert!(*val < senders);
        }

        for handle in handles {
            let _ = handle.join();
        }
    })
}

fn main() {
    let stats = example(3);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
