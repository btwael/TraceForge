use traceforge_rounds::{Comm, CommError, Dim, Round, Transport};

pub mod udp;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
pub enum Phase {
    Vote,
    Decision,
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
pub struct TwoPcRound {
    instance: u32,
    phase: Phase,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Msg<N> {
    Prepare { coordinator: N },
    Vote(bool),
    Decision(bool),
}

pub fn coordinator<T, N>(
    comm: &mut Comm<TwoPcRound, T>,
    coordinator: N,
    participants: &[N],
    num_rounds: u32,
) -> Result<(), CommError<TwoPcRound, Msg<N>, <T as Transport<TwoPcRound, Msg<N>>>::Error>>
where
    T: Transport<TwoPcRound, Msg<N>, Node = N>,
    N: Copy + 'static,
{
    coordinator_with_decisions(comm, coordinator, participants, num_rounds, |_, _| {})
}

pub fn coordinator_with_decisions<T, N, Decision>(
    comm: &mut Comm<TwoPcRound, T>,
    coordinator: N,
    participants: &[N],
    num_rounds: u32,
    mut decision: Decision,
) -> Result<(), CommError<TwoPcRound, Msg<N>, <T as Transport<TwoPcRound, Msg<N>>>::Error>>
where
    T: Transport<TwoPcRound, Msg<N>, Node = N>,
    N: Copy + 'static,
    Decision: FnMut(u32, bool),
{
    for round in 0..num_rounds {
        for participant in participants.iter().copied() {
            comm.send(participant, Msg::Prepare { coordinator })
                .map_err(CommError::Transport)?;
        }

        let votes = comm.inbox_with_bounds_with::<Msg<N>, _>(
            participants.len(),
            Some(participants.len()),
            |local, remote| local == remote,
        )?;
        let commit = votes
            .into_iter()
            .all(|entry| matches!(entry, Some(Msg::Vote(true))));
        decision(round, commit);

        comm.rounds().tick();
        for participant in participants.iter().copied() {
            comm.send(participant, Msg::Decision(commit))
                .map_err(CommError::Transport)?;
        }

        comm.rounds().tick();
    }

    Ok(())
}

pub fn participant<T, N, Vote>(
    comm: &mut Comm<TwoPcRound, T>,
    num_rounds: u32,
    mut vote_for_round: Vote,
) -> Result<(), CommError<TwoPcRound, Msg<N>, <T as Transport<TwoPcRound, Msg<N>>>::Error>>
where
    T: Transport<TwoPcRound, Msg<N>, Node = N>,
    N: Copy + 'static,
    Vote: FnMut(u32) -> bool,
{
    for round in 0..num_rounds {
        let prepare = comm.recv_block_with::<Msg<N>, _>(|local, remote| local == remote)?;

        let Msg::Prepare { coordinator } = prepare else {
            panic!("expected prepare message");
        };

        let vote_yes = vote_for_round(round);
        comm.send(coordinator, Msg::Vote(vote_yes))
            .map_err(CommError::Transport)?;

        comm.rounds().tick();
        let decision = comm.recv_block_with::<Msg<N>, _>(|local, remote| local == remote)?;

        match decision {
            Msg::Decision(true) => assert!(vote_yes),
            Msg::Decision(false) => {}
            _ => panic!("expected decision message"),
        }

        comm.rounds().tick();
    }

    Ok(())
}
