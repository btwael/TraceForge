use std::sync::Arc;

use traceforge::comm_close::{self, RoundScheme, RoundStamp, Rounds, TagCmp};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const NUM_PARTICIPANTS: usize = 3;
const NUM_ROUNDS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Participants {
    nodes: [ThreadId; NUM_PARTICIPANTS],
}

impl Participants {
    fn from_vec(nodes: Vec<ThreadId>) -> Self {
        let nodes: [ThreadId; NUM_PARTICIPANTS] = nodes
            .try_into()
            .unwrap_or_else(|_| panic!("expected {} participants", NUM_PARTICIPANTS));
        Self { nodes }
    }

    fn len(&self) -> usize {
        self.nodes.len()
    }

    fn iter(&self) -> std::slice::Iter<'_, ThreadId> {
        self.nodes.iter()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vote {
    Yes,
    No,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Commit,
    Abort,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrepareMsg {
    stamp: RoundStamp,
    coordinator: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VoteMsg {
    stamp: RoundStamp,
    vote: Vote,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DecideMsg {
    stamp: RoundStamp,
    decision: Decision,
    sender: ThreadId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InitMsg {
    coordinator: ThreadId,
    participants: Participants,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(InitMsg),
    Prepare(PrepareMsg),
    Vote(VoteMsg),
    Decide(DecideMsg),
}

type RoundCollector = dyn Fn(
        &comm_close::Round,
        &comm_close::RoundFilter,
        usize,
        Option<usize>,
    ) -> Vec<Message>
    + Send
    + Sync;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Coordinator,
    Participant,
}

struct Node {
    role: Role,
    participants: Participants,
    coordinator: ThreadId,
    me: ThreadId,
    rounds: Rounds,
    started: bool,
}

impl Node {
    fn new(init: InitMsg, scheme: RoundScheme) -> Self {
        let me = thread::current().id();
        let role = if me == init.coordinator {
            Role::Coordinator
        } else {
            Role::Participant
        };
        let rounds = Rounds::with_scheme(scheme);
        Self {
            role,
            participants: init.participants,
            coordinator: init.coordinator,
            me,
            rounds,
            started: false,
        }
    }

    fn run(mut self, collect: &RoundCollector) {
        match self.role {
            Role::Coordinator => self.run_coordinator(collect),
            Role::Participant => self.run_participant(collect),
        }
    }

    fn run_coordinator(&mut self, collect: &RoundCollector) {
        for _ in 0..NUM_ROUNDS {
            let prepare_round = self.next_round();
            let prepare = PrepareMsg {
                stamp: prepare_round.stamp(),
                coordinator: self.me,
                sender: self.me,
            };
            self.broadcast_participants(&prepare_round, Message::Prepare(prepare));

            let votes = self.collect_votes(&prepare_round, collect, self.participants.len());
            let all_yes = votes.iter().all(|msg| msg.vote == Vote::Yes);
            let decision = if all_yes {
                Decision::Commit
            } else {
                Decision::Abort
            };

            let decision_round = self.rounds.advance_level(1);
            let decide = DecideMsg {
                stamp: decision_round.stamp(),
                decision,
                sender: self.me,
            };
            self.broadcast_participants(&decision_round, Message::Decide(decide));
        }
    }

    fn run_participant(&mut self, collect: &RoundCollector) {
        for _ in 0..NUM_ROUNDS {
            let prepare_round = self.next_round();
            let prepare = self.collect_prepare(&prepare_round, collect);

            let vote_yes = traceforge::nondet();
            let vote = if vote_yes { Vote::Yes } else { Vote::No };
            let vote_msg = VoteMsg {
                stamp: prepare_round.stamp(),
                vote,
                sender: self.me,
            };
            comm_close::send(prepare.coordinator, Message::Vote(vote_msg), &prepare_round);

            let decision_round = self.rounds.advance_level(1);
            let decision = self.collect_decide(&decision_round, collect);
            match decision.decision {
                Decision::Commit => assert!(vote_yes),
                Decision::Abort => (),
            }
        }
    }

    fn next_round(&mut self) -> comm_close::Round {
        if self.started {
            self.rounds.advance_round()
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn broadcast_participants(&self, round: &comm_close::Round, msg: Message) {
        for node in self.participants.iter() {
            if *node != self.me {
                comm_close::send(*node, msg.clone(), round);
            }
        }
    }

    fn collect_prepare(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
    ) -> PrepareMsg {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        let mut msgs = collect(round, &filter, 1, Some(1));
        let msg = msgs.pop().expect("expected prepare message");
        match msg {
            Message::Prepare(payload) => payload,
            _ => panic!("expected PrepareMsg"),
        }
    }

    fn collect_votes(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        expected: usize,
    ) -> Vec<VoteMsg> {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, expected, Some(expected))
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Vote(payload) => Some(payload),
                _ => panic!("expected VoteMsg"),
            })
            .collect()
    }

    fn collect_decide(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
    ) -> DecideMsg {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        let mut msgs = collect(round, &filter, 1, Some(1));
        let msg = msgs.pop().expect("expected decision message");
        match msg {
            Message::Decide(payload) => payload,
            _ => panic!("expected DecideMsg"),
        }
    }
}

fn start_node(collect: &RoundCollector, scheme: RoundScheme) {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let init = match init {
        Message::Init(init) => init,
        _ => panic!("expected init message"),
    };
    Node::new(init, scheme).run(collect);
}

fn run_protocol(collect: Arc<RoundCollector>) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().with_lossy(2).build(), move || {
        let scheme = RoundScheme::builder()
            .level("round", TagCmp::Gte)
            .level("phase", TagCmp::Eq)
            .build();

        let mut handles = Vec::new();
        for _ in 0..(NUM_PARTICIPANTS + 1) {
            let receive = collect.clone();
            let scheme = scheme.clone();
            handles.push(thread::spawn(move || start_node(receive.as_ref(), scheme)));
        }

        let coordinator = handles[0].thread().id();
        let participants = Participants::from_vec(
            handles[1..]
                .iter()
                .map(|h| h.thread().id())
                .collect(),
        );
        let init = InitMsg {
            coordinator,
            participants,
        };
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(init));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    })
}

fn run_protocol_with_recv() -> traceforge::Stats {
    let collect: Arc<RoundCollector> = Arc::new(|round, filter, min, max| {
        let upper = max.expect("requires bounded max");
        assert!(upper >= min, "requires max >= min");
        let count = if upper == min {
            min
        } else {
            (min..=upper).nondet()
        };

        let mut out = Vec::new();
        for _ in 0..count {
            let msg = comm_close::recv_block_with_filter::<Message>(filter);
            out.push(msg.payload(round).clone());
        }
        out
    });
    run_protocol(collect)
}

fn run_protocol_with_inbox() -> traceforge::Stats {
    let collect: Arc<RoundCollector> = Arc::new(|round, filter, min, max| {
        comm_close::inbox_with_bounds_filter(filter, min, max)
            .into_iter()
            .flatten()
            .filter_map(|msg| {
                msg.payload(round)
                    .as_any_ref()
                    .downcast_ref::<Message>()
                    .cloned()
            })
            .collect()
    });
    run_protocol(collect)
}

fn main() {
    let use_recv = std::env::args().any(|arg| arg == "recv");
    let use_inbox = std::env::args().any(|arg| arg == "inbox");

    if use_recv && use_inbox {
        panic!("Can't use recv/inbox at the same time!");
    } else if !use_recv && !use_inbox {
        panic!("Must specify recv or inbox!");
    }

    let stats = if use_recv {
        run_protocol_with_recv()
    } else {
        run_protocol_with_inbox()
    };
    println!("Stats = {}, {}", stats.execs, stats.block);
}
