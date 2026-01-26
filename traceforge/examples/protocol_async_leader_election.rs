use std::sync::Arc;

use traceforge::comm_close::{self, RoundScheme, RoundStamp, Rounds, TagCmp};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const NUM_NODES: usize = 2;
const NUM_BALLOTS: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Participants {
    nodes: [ThreadId; NUM_NODES],
}

impl Participants {
    fn from_vec(nodes: Vec<ThreadId>) -> Self {
        let nodes: [ThreadId; NUM_NODES] = nodes
            .try_into()
            .unwrap_or_else(|_| panic!("expected {} participants", NUM_NODES));
        Self { nodes }
    }

    fn len(&self) -> usize {
        self.nodes.len()
    }

    fn iter(&self) -> std::slice::Iter<'_, ThreadId> {
        self.nodes.iter()
    }

    fn get(&self, idx: usize) -> ThreadId {
        self.nodes[idx]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    NewBallot,
    AckBallot,
}

impl Phase {
    fn tag(self) -> u32 {
        match self {
            Phase::NewBallot => 0,
            Phase::AckBallot => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NewBallotMsg {
    ballot: u32,
    stamp: RoundStamp,
    leader: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AckBallotMsg {
    ballot: u32,
    stamp: RoundStamp,
    leader: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    NewBallot(NewBallotMsg),
    AckBallot(AckBallotMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogEntry {
    ballot: u32,
    leader: ThreadId,
}

type RoundCollector = dyn Fn(
        &comm_close::Round,
        &comm_close::RoundFilter,
        usize,
        Option<usize>,
    ) -> Vec<Message>
    + Send
    + Sync;

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds,
    ballot: u32,
    leader: ThreadId,
    started: bool,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
        Self {
            nodes,
            me,
            rounds,
            ballot: 0,
            leader: me,
            started: false,
        }
    }

    fn run(mut self, collect: &RoundCollector) -> Vec<LogEntry> {
        let mut log = Vec::new();
        for _ in 0..NUM_BALLOTS {
            self.step(collect, &mut log);
        }
        log
    }

    fn step(&mut self, collect: &RoundCollector, log: &mut Vec<LogEntry>) {
        let nb_round = self.next_round();

        if self.coord() {
            let msg = NewBallotMsg {
                ballot: self.ballot,
                stamp: nb_round.stamp(),
                leader: self.me,
                sender: self.me,
            };
            self.broadcast(&nb_round, Message::NewBallot(msg));
            self.leader = self.me;
        } else {
            let nb_msgs = self.collect_new_ballot(&nb_round, collect);
            if nb_msgs.len() == 1 {
                let msg = &nb_msgs[0];
                if msg.stamp.gt_at(&nb_round.stamp(), 0) {
                    self.ballot = msg.stamp.components()[0];
                    self.rounds.jump(&msg.stamp);
                }
                self.leader = msg.leader;
            }
        }

        // AckBallot step (common)
        let ack_round = self.rounds.advance_level(1);
        let stamp = ack_round.stamp();
        let ack = AckBallotMsg {
            ballot: self.ballot,
            stamp,
            leader: self.leader,
            sender: self.me,
        };
        self.broadcast(&ack_round, Message::AckBallot(ack));

        let recv_max = self.nodes.len() - 1;
        let ack_msgs = self.collect_ack_ballot(&ack_round, self.ballot, collect, recv_max);
        if ack_msgs.len() > self.nodes.len() / 2 && Self::all_same_leader(&ack_msgs, self.leader) {
            log.push(LogEntry {
                ballot: self.ballot,
                leader: self.leader,
            });
        }
    }

    fn next_round(&mut self) -> comm_close::Round {
        if self.started {
            self.ballot += 1;
            self.rounds.advance_round()
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn coord(&self) -> bool {
        // TODO: remodel
        return traceforge::nondet();
    }

    fn broadcast(&self, round: &comm_close::Round, msg: Message) {
        for node in self.nodes.iter() {
            if *node != self.me {
                comm_close::send(*node, msg.clone(), round);
            }
        }
    }

    fn collect_new_ballot(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
    ) -> Vec<NewBallotMsg> {
        let filter = round.filter();
        collect(round, &filter, 0, Some(1))
            .into_iter()
            .filter_map(|msg| match msg {
                Message::NewBallot(payload) => Some(payload),
                _ => panic!("expected NewBallotMsg"),
            })
            .collect()
    }

    fn collect_ack_ballot(
        &self,
        round: &comm_close::Round,
        ballot: u32,
        collect: &RoundCollector,
        max: usize,
    ) -> Vec<AckBallotMsg> {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, 0, Some(max))
            .into_iter()
            .filter_map(|msg| match msg {
                Message::AckBallot(payload) if payload.stamp.components()[0] == ballot => {
                    Some(payload)
                }
                _ => panic!("expected AckBallotMsg with {}", ballot),
            })
            .collect()
    }

    fn all_same_leader(messages: &[AckBallotMsg], leader: ThreadId) -> bool {
        messages.iter().all(|msg| msg.leader == leader)
    }
}

fn start_node(collect: &RoundCollector, scheme: RoundScheme) -> Vec<LogEntry> {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, scheme).run(collect)
}

fn assert_log_consistency(logs: &[Vec<LogEntry>]) {
    for ballot in 1..=NUM_BALLOTS {
        let mut chosen: Option<ThreadId> = None;
        for log in logs {
            for entry in log.iter().filter(|entry| entry.ballot == ballot) {
                if let Some(prev) = chosen {
                    assert_eq!(prev, entry.leader);
                } else {
                    chosen = Some(entry.leader);
                }
            }
        }
    }
}

fn run_protocol(collect: Arc<RoundCollector>) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let scheme = RoundScheme::builder()
            .level("round", TagCmp::Gte)
            .level("phase", TagCmp::Eq)
            .build();
        let mut handles = Vec::new();
        for _ in 0..NUM_NODES {
            let receive = collect.clone();
            let scheme = scheme.clone();
            handles.push(thread::spawn(move || start_node(receive.as_ref(), scheme)));
        }
        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(nodes));
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }
        assert_log_consistency(&logs);
    })
}

fn run_protocol_with_recv() -> traceforge::Stats {
    let collect: Arc<RoundCollector> = Arc::new(|round, filter, min, max| {
        let upper = match max {
            Some(upper) => upper,
            None => NUM_BALLOTS as usize * NUM_NODES,
        };
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

    if (use_recv && use_inbox) {
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
