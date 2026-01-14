use traceforge::comm_close::{self, Rounds};
use traceforge::thread;

fn example() -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), || {
        let main_id = thread::main_thread_id();
        let mut rounds = Rounds::new();

        let sender_round0 = thread::spawn(move || {
            let rounds = Rounds::new();
            let round = rounds.current();
            comm_close::send(main_id, 10_u32, &round);
        });

        {
            let round = rounds.current();
            let msg = comm_close::recv_block::<u32>(&round);
            msg.with_payload(&round, |val| assert_eq!(*val, 10));
        }

        let _ = sender_round0.join();

        let sender_round1 = thread::spawn(move || {
            let mut rounds = Rounds::new();
            let round = rounds.advance();
            comm_close::send(main_id, 20_u32, &round);
            comm_close::send(main_id, 30_u32, &round);
        });

        let _ = sender_round1.join();

        {
            let round = rounds.advance();
            let messages = comm_close::inbox(&round);
            for msg in messages.into_iter().flatten() {
                let val = msg
                    .payload(&round)
                    .as_any_ref()
                    .downcast_ref::<u32>()
                    .expect("expected a u32 payload");
                assert!(*val == 20 || *val == 30);
            }
        }
    })
}

fn main() {
    let stats = example();
    println!("Stats = {}, {}", stats.execs, stats.block);
}
