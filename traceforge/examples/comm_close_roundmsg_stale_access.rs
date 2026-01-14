// This example is expected to panic at runtime.
// It stores RoundMsg values and tries to access them using a later round token.
use traceforge::comm_close::{self, Rounds};
use traceforge::thread;

fn main() {
    traceforge::verify(traceforge::Config::builder().build(), || {
        let main_id = thread::main_thread_id();
        let sender = thread::spawn(move || {
            let rounds = Rounds::new();
            let round = rounds.current();
            comm_close::send(main_id, 1_u32, &round);
        });

        let mut rounds = Rounds::new();
        let mut stored = Vec::new();

        {
            let round = rounds.current();
            let msg = comm_close::recv_block::<u32>(&round);
            stored.push(msg);
        }

        let _ = rounds.advance();

        let mut rounds2 = Rounds::new();
        let later_round = rounds2.current();
        for msg in &stored {
            let _ = msg.payload(&later_round);
        }

        let _ = sender.join();
    });
}
