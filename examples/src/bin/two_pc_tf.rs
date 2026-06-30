use examples::two_pc::{self, TwoPcRound};
use traceforge::thread::{self, ThreadId};
use traceforge::Config;

const DEFAULT_NUM_PARTICIPANTS: usize = 3;
const DEFAULT_NUM_ROUNDS: u32 = 3;

fn coordinator(participants: Vec<ThreadId>) {
    let mut comm = traceforge::comm_close::comm::<TwoPcRound>();
    two_pc::coordinator(
        &mut comm,
        thread::current_id(),
        &participants,
        DEFAULT_NUM_ROUNDS,
    )
    .unwrap();
}

fn participant() {
    let mut comm = traceforge::comm_close::comm::<TwoPcRound>();
    two_pc::participant(&mut comm, DEFAULT_NUM_ROUNDS, |_| traceforge::nondet()).unwrap();
}

fn protocol() {
    let participants = (0..DEFAULT_NUM_PARTICIPANTS)
        .map(|_| thread::spawn(participant))
        .collect::<Vec<_>>();
    let participant_ids = participants
        .iter()
        .map(|participant| participant.thread().id())
        .collect::<Vec<_>>();

    let coordinator = thread::spawn(move || coordinator(participant_ids));
    coordinator.join().unwrap();
    for participant in participants {
        participant.join().unwrap();
    }
}

fn main() {
    let stats = traceforge::verify(Config::builder().build(), protocol);
    println!(
        "2PC comm_close: {} complete, {} blocked",
        stats.execs, stats.block
    );
}
