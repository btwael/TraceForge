use traceforge::{comm_close, thread, Config};
use traceforge_rounds::{set::KeyScheme, Round};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum Slot {
    A,
    B,
}

impl KeyScheme for Slot {
    const LEN: usize = 1;

    fn encode(&self, out: &mut Vec<u32>) {
        out.push(match self {
            Self::A => 1,
            Self::B => 2,
        });
    }

    fn decode(raw: &[u32]) -> Option<Self> {
        if raw.len() != Self::LEN {
            return None;
        }
        match raw[0] {
            1 => Some(Self::A),
            2 => Some(Self::B),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
struct LocalRound {
    phase: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Msg {
    A,
    B,
}

#[test]
fn set_comm_rounds_are_independent_per_key() {
    let stats = traceforge::verify(Config::builder().build(), || {
        let receiver = thread::spawn(move || {
            let mut comm = comm_close::set::<Slot, LocalRound>();
            let msg = comm.on(Slot::A).recv_block::<Msg>().unwrap();
            assert_eq!(msg, Msg::A);
        });
        let receiver_id = receiver.thread().id();

        let sender = thread::spawn(move || {
            let mut comm = comm_close::set::<Slot, LocalRound>();
            comm.on(Slot::B).rounds().advance(LocalRound::dim_phase());
            comm.on(Slot::A).send(receiver_id, Msg::A).unwrap();
        });

        sender.join().unwrap();
        receiver.join().unwrap();
    });

    assert_eq!(stats.block, 0);
    assert!(stats.execs > 0);
}

#[test]
fn set_comm_inbox_is_scoped_to_key() {
    let stats = traceforge::verify(Config::builder().build(), || {
        let receiver = thread::spawn(move || {
            let mut comm = comm_close::set::<Slot, LocalRound>();
            let slot_a = comm
                .on(Slot::A)
                .inbox_with_bounds::<Msg>(1, Some(2))
                .unwrap();
            let collected = slot_a.into_iter().flatten().collect::<Vec<_>>();
            assert_eq!(collected, vec![Msg::A]);

            let slot_b = comm.on(Slot::B).recv_block::<Msg>().unwrap();
            assert_eq!(slot_b, Msg::B);
        });
        let receiver_id = receiver.thread().id();

        let sender = thread::spawn(move || {
            let mut comm = comm_close::set::<Slot, LocalRound>();
            comm.on(Slot::A).send(receiver_id, Msg::A).unwrap();
            comm.on(Slot::B).send(receiver_id, Msg::B).unwrap();
        });

        sender.join().unwrap();
        receiver.join().unwrap();
    });

    assert_eq!(stats.block, 0);
    assert!(stats.execs > 0);
}

#[test]
fn set_comm_rejects_untagged_transport() {
    let mut comm = comm_close::set_with::<Slot, LocalRound>(
        comm_close::TraceForgeTransportMode::UntaggedRepeatedRecv,
    );
    let err = comm
        .on(Slot::A)
        .send(thread::construct_thread_id(0), Msg::A)
        .unwrap_err();
    assert_eq!(
        err,
        comm_close::TraceForgeSetTransportError::UnsupportedMode(
            comm_close::TraceForgeTransportMode::UntaggedRepeatedRecv
        )
    );
}
