use std::sync::Arc;

use traceforge::comm_close::{self, RoundScheme, RoundStamp, Rounds, TagCmp};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

// Keep bounds explicit and small for verification.
const NUM_NODES: usize = 3;
const NUM_ROUNDS: u32 = 3;

// Chandra-Toueg uses a quorum of size (n + 1) / 2.
const QUORUM: usize = (NUM_NODES + 1) / 2;

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
    First,
    Second,
    Third,
    Fourth,
}

impl Phase {
    fn tag(self) -> u32 {
        match self {
            Phase::First => 0,
            Phase::Second => 1,
            Phase::Third => 2,
            Phase::Fourth => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FirstPhaseMsg {
    stamp: RoundStamp,
    estimate: i32,
    timestamp: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SecondPhaseMsg {
    stamp: RoundStamp,
    estimate: i32,
    timestamp: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThirdPhaseMsg {
    stamp: RoundStamp,
    estimate: i32,
    timestamp: u32,
    ack: i32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DecideMsg {
    stamp: RoundStamp,
    estimate: i32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    First(FirstPhaseMsg),
    Second(SecondPhaseMsg),
    Third(ThirdPhaseMsg),
    Decide(DecideMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogEntry {
    round: u32,
    estimate: i32,
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

    estimate: i32,
    timestamp: u32,
    decided: bool,

    // Used only to bound the outer "round" loop.
    started: bool,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);

        // Domain of initial values is intentionally tiny for verification.
        let estimate = if traceforge::nondet() { 0 } else { 1 };

        Self {
            nodes,
            me,
            rounds,
            estimate,
            timestamp: 0,
            decided: false,
            started: false,
        }
    }

    fn run(mut self, collect: &RoundCollector) -> Vec<LogEntry> {
        let mut log = Vec::new();
        for _ in 0..NUM_ROUNDS {
            if self.decided {
                break;
            }
            self.step_round(collect, &mut log);
        }
        log
    }

    fn step_round(&mut self, collect: &RoundCollector, log: &mut Vec<LogEntry>) {
        // Outer loop: "round" (monotone).
        // Inner component: "phase" (First..Fourth) within the current round.
        let first_phase = self.next_round();
        let round_id = self.round_of(&first_phase.stamp());
        let leader = self.leader_for_round(round_id);

        // --- Phase 1: send (estimate,timestamp) to leader ---
        if leader != self.me {
            let msg = FirstPhaseMsg {
                stamp: first_phase.stamp(),
                estimate: self.estimate,
                timestamp: self.timestamp,
                sender: self.me,
            };
            comm_close::send(leader, Message::First(msg), &first_phase);
        }

        if self.me == leader {
            // Include local state as if we had sent to ourselves.
            let mut mbox = vec![FirstPhaseMsg {
                stamp: first_phase.stamp(),
                estimate: self.estimate,
                timestamp: self.timestamp,
                sender: self.me,
            }];

            // Collect enough messages to reach a quorum.
            let min_other = QUORUM.saturating_sub(1);
            let max_other = self.nodes.len().saturating_sub(1);
            let mut recvd =
                self.collect_first_phase(&first_phase, collect, min_other, Some(max_other));
            mbox.append(&mut recvd);

            let chosen = Self::max_timestamp(&mbox);
            self.estimate = chosen.estimate;
        }

        // --- Phase 2: leader broadcasts estimate; receivers set timestamp=round_id and ack=1 ---
        let second_phase = self.rounds.advance_level(1);
        let mut ack: i32 = 0;

        if self.me == leader {
            let msg = SecondPhaseMsg {
                stamp: second_phase.stamp(),
                estimate: self.estimate,
                timestamp: self.timestamp,
                sender: self.me,
            };
            self.broadcast(&second_phase, Message::Second(msg));

            // Leader "receives" its own value in this phase.
            self.timestamp = round_id;
            ack = 1;
        } else {
            let msgs = self.collect_second_phase(&second_phase, collect, 0, Some(1));
            if msgs.len() == 1 {
                let msg = &msgs[0];
                self.estimate = msg.estimate;
                self.timestamp = round_id;
                ack = 1;
            } else {
                // Timeout branch.
                ack = -1;
            }
        }

        // --- Phase 3: send (estimate,timestamp,ack) to leader; leader checks all_ack on quorum ---
        let third_phase = self.rounds.advance_level(1);

        if self.me != leader {
            let msg = ThirdPhaseMsg {
                stamp: third_phase.stamp(),
                estimate: self.estimate,
                timestamp: self.timestamp,
                ack,
                sender: self.me,
            };
            comm_close::send(leader, Message::Third(msg), &third_phase);
        }

        let leader_ack = if self.me == leader {
            let mut mbox = vec![ThirdPhaseMsg {
                stamp: third_phase.stamp(),
                estimate: self.estimate,
                timestamp: self.timestamp,
                ack,
                sender: self.me,
            }];

            let min_other = QUORUM.saturating_sub(1);
            let max_other = self.nodes.len().saturating_sub(1);
            let mut recvd =
                self.collect_third_phase(&third_phase, collect, min_other, Some(max_other));
            mbox.append(&mut recvd);

            if Self::all_ack(&mbox) {
                1
            } else {
                -1
            }
        } else {
            0
        };

        // --- Phase 4: if leader_ack==1, leader broadcasts DECIDE; everybody may receive DECIDE ---
        let fourth_phase = self.rounds.advance_level(1);

        if self.me == leader && leader_ack == 1 {
            let msg = DecideMsg {
                stamp: fourth_phase.stamp(),
                estimate: self.estimate,
                sender: self.me,
            };
            self.broadcast(&fourth_phase, Message::Decide(msg));
            self.decide(round_id, self.estimate, log);
            return;
        }

        // Non-leaders (and leaders with leader_ack != 1) may time out waiting for DECIDE.
        let decides = self.collect_decide(&fourth_phase, collect, 0, Some(1));
        if decides.len() == 1 {
            let msg = &decides[0];
            self.decide(self.round_of(&msg.stamp), msg.estimate, log);
        }
    }

    fn next_round(&mut self) -> comm_close::Round {
        if self.started {
            self.rounds.advance_round()
        } else {
            self.started = true;
            self.rounds.advance_round()
        }
    }

    fn round_of(&self, stamp: &RoundStamp) -> u32 {
        stamp.components()[0]
    }

    fn phase_of(&self, stamp: &RoundStamp) -> u32 {
        stamp.components()[1]
    }

    fn leader_for_round(&self, round_id: u32) -> ThreadId {
        // Rotate leader by round.
        let idx = (round_id as usize) % self.nodes.len();
        self.nodes.get(idx)
    }

    fn broadcast(&self, round: &comm_close::Round, msg: Message) {
        for node in self.nodes.iter() {
            if *node != self.me {
                comm_close::send(*node, msg.clone(), round);
            }
        }
    }

    fn decide(&mut self, round_id: u32, estimate: i32, log: &mut Vec<LogEntry>) {
        if self.decided {
            return;
        }
        self.decided = true;
        log.push(LogEntry {
            round: round_id,
            estimate,
        });
    }

    fn collect_first_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<FirstPhaseMsg> {
        // Only accept Phase-1 messages for *this* round (ignore future-round Phase-1 messages).
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::First(payload) => Some(payload),
                _ => panic!("expected FirstPhaseMsg"),
            })
            .collect()
    }

    fn collect_second_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<SecondPhaseMsg> {
        // Only accept Phase-2 messages for *this* round.
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Second(payload) => Some(payload),
                _ => panic!("expected SecondPhaseMsg"),
            })
            .collect()
    }

    fn collect_third_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<ThirdPhaseMsg> {
        // Only accept Phase-3 messages for *this* round.
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Third(payload) => Some(payload),
                _ => panic!("expected ThirdPhaseMsg"),
            })
            .collect()
    }

    fn collect_decide(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<DecideMsg> {
        // For DECIDE we keep the scheme's default "round" comparator (Gte), so a process may
        // accept a decision from a later round if it is behind.
        let filter = round.filter();
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Decide(payload) => Some(payload),
                _ => panic!("expected DecideMsg"),
            })
            .collect()
    }

    fn max_timestamp(messages: &[FirstPhaseMsg]) -> &FirstPhaseMsg {
        messages
            .iter()
            .max_by_key(|m| m.timestamp)
            .expect("requires non-empty quorum")
    }

    fn all_ack(messages: &[ThirdPhaseMsg]) -> bool {
        messages.iter().all(|m| m.ack == 1)
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

fn assert_agreement(logs: &[Vec<LogEntry>]) {
    let mut chosen: Option<i32> = None;

    for log in logs {
        for entry in log {
            if let Some(prev) = chosen {
                assert_eq!(prev, entry.estimate);
            } else {
                chosen = Some(entry.estimate);
            }
        }
    }
}

fn run_protocol(collect: Arc<RoundCollector>) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        // Tag layout (outer -> inner): (round, phase).
        // - round: grows monotonically, so we use Gte by default;
        // - phase: bounded step tag (First..Fourth), so we use Eq by default.
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
        assert_agreement(&logs);
    })
}

fn run_protocol_with_recv() -> traceforge::Stats {
    let collect: Arc<RoundCollector> = Arc::new(|round, filter, min, max| {
        let upper = match max {
            Some(upper) => upper,
            None => (NUM_ROUNDS as usize) * 4 * NUM_NODES,
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
