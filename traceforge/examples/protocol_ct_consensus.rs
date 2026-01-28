use std::sync::Arc;

use traceforge::comm_close::{self, RoundScheme, RoundStamp, Rounds, TagCmp};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_ROUNDS: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Participants {
    nodes: Vec<ThreadId>,
}

impl Participants {
    fn from_vec(nodes: Vec<ThreadId>) -> Self {
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
    estimate: bool,
    timestamp: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SecondPhaseMsg {
    stamp: RoundStamp,
    estimate: bool,
    timestamp: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThirdPhaseMsg {
    stamp: RoundStamp,
    estimate: bool,
    timestamp: u32,
    ack: bool,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DecideMsg {
    stamp: RoundStamp,
    estimate: bool,
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
    estimate: bool,
}

type RoundCollector = dyn Fn(
        &comm_close::Round,
        &comm_close::RoundFilter,
        usize,
        Option<usize>,
    ) -> Vec<comm_close::RoundMsg<Message>>
    + Send
    + Sync;

enum CollectOutcome<T> {
    Decide(comm_close::RoundMsg<Message>),
    Messages(Vec<T>),
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds,
    num_rounds: u32,
    quorum: usize,

    estimate: bool,
    timestamp: u32,
    decided: bool,

    // Used only to bound the outer "round" loop.
    started: bool,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme, num_rounds: u32) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
        let quorum = (nodes.len() + 1) / 2;

        // Domain of initial values is intentionally tiny for verification.
        let estimate = traceforge::nondet();

        Self {
            nodes,
            me,
            rounds,
            num_rounds,
            quorum,
            estimate,
            timestamp: 0,
            decided: false,
            started: false,
        }
    }

    fn run(mut self, collect: &RoundCollector) -> Vec<LogEntry> {
        let mut log = Vec::new();
        for _ in 0..self.num_rounds {
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
        let leader = self.coord(round_id);

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
            let min_other = self.quorum.saturating_sub(1);
            let mut recvd = match self.collect_first_phase(
                &first_phase,
                collect,
                min_other,
                Some(min_other),
            ) {
                CollectOutcome::Decide(msg) => {
                    let stamp = msg.round_stamp();
                    let jumped = self.rounds.jump(&stamp);
                    let payload = msg.payload(&jumped);
                    let Message::Decide(decide) = payload else {
                        panic!("expected DecideMsg");
                    };
                    self.decide(self.round_of(&decide.stamp), decide.estimate, log);
                    return;
                }
                CollectOutcome::Messages(msgs) => msgs,
            };
            mbox.append(&mut recvd);

            let chosen = Self::max_timestamp(&mbox);
            self.estimate = chosen.estimate;
        }

        // --- Phase 2: leader broadcasts estimate; receivers set timestamp=round_id and ack=1 ---
        let second_phase = self.rounds.advance_level(1);
        let mut ack: bool = false;

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
            ack = true;
        } else {
            let msgs = match self.collect_second_phase(&second_phase, collect, 0, Some(1)) {
                CollectOutcome::Decide(msg) => {
                    let stamp = msg.round_stamp();
                    let jumped = self.rounds.jump(&stamp);
                    let payload = msg.payload(&jumped);
                    let Message::Decide(decide) = payload else {
                        panic!("expected DecideMsg");
                    };
                    self.decide(self.round_of(&decide.stamp), decide.estimate, log);
                    return;
                }
                CollectOutcome::Messages(msgs) => msgs,
            };
            if msgs.len() == 1 {
                let msg = &msgs[0];
                self.estimate = msg.estimate;
                self.timestamp = round_id;
                ack = true;
            } else {
                // Timeout branch.
                ack = false;
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

            let min_other = self.quorum.saturating_sub(1);
            let mut recvd = match self.collect_third_phase(
                &third_phase,
                collect,
                min_other,
                Some(min_other),
            ) {
                CollectOutcome::Decide(msg) => {
                    let stamp = msg.round_stamp();
                    let jumped = self.rounds.jump(&stamp);
                    let payload = msg.payload(&jumped);
                    let Message::Decide(decide) = payload else {
                        panic!("expected DecideMsg");
                    };
                    self.decide(self.round_of(&decide.stamp), decide.estimate, log);
                    return;
                }
                CollectOutcome::Messages(msgs) => msgs,
            };
            mbox.append(&mut recvd);

            Self::all_ack(&mbox)
        } else {
            false
        };

        // --- Phase 4: if leader_ack==1, leader broadcasts DECIDE; everybody may receive DECIDE ---
        let fourth_phase = self.rounds.advance_level(1);

        if self.me == leader && leader_ack {
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

    fn coord(&self, round_id: u32) -> ThreadId {
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

    fn decide(&mut self, round_id: u32, estimate: bool, log: &mut Vec<LogEntry>) {
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
    ) -> CollectOutcome<FirstPhaseMsg> {
        // Accept current Phase-1 messages, but allow early DECIDE from any round.
        let stamp = round.stamp();
        let round_id = self.round_of(&stamp);
        let phase_tag = self.phase_of(&stamp);
        let filter = round
            .filter()
            .level_cmp(0, TagCmp::Eq)
            .or_pattern(|p| {
                p.level_cmp(0, TagCmp::Gte)
                    .level_eq_value(1, Phase::Fourth.tag())
            });

        let mut out = Vec::new();
        for msg in collect(round, &filter, min, max) {
            let msg_stamp = msg.round_stamp();
            if self.phase_of(&msg_stamp) == Phase::Fourth.tag() {
                return CollectOutcome::Decide(msg);
            }
            if self.round_of(&msg_stamp) == round_id && self.phase_of(&msg_stamp) == phase_tag {
                match msg.payload(round) {
                    Message::First(payload) => out.push(payload.clone()),
                    _ => panic!("expected FirstPhaseMsg"),
                }
            }
        }
        CollectOutcome::Messages(out)
    }

    fn collect_second_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> CollectOutcome<SecondPhaseMsg> {
        // Accept current Phase-2 messages, but allow early DECIDE from any round.
        let stamp = round.stamp();
        let round_id = self.round_of(&stamp);
        let phase_tag = self.phase_of(&stamp);
        let filter = round
            .filter()
            .level_cmp(0, TagCmp::Eq)
            .or_pattern(|p| {
                p.level_cmp(0, TagCmp::Gte)
                    .level_eq_value(1, Phase::Fourth.tag())
            });

        let mut out = Vec::new();
        for msg in collect(round, &filter, min, max) {
            let msg_stamp = msg.round_stamp();
            if self.phase_of(&msg_stamp) == Phase::Fourth.tag() {
                return CollectOutcome::Decide(msg);
            }
            if self.round_of(&msg_stamp) == round_id && self.phase_of(&msg_stamp) == phase_tag {
                match msg.payload(round) {
                    Message::Second(payload) => out.push(payload.clone()),
                    _ => panic!("expected SecondPhaseMsg"),
                }
            }
        }
        CollectOutcome::Messages(out)
    }

    fn collect_third_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> CollectOutcome<ThirdPhaseMsg> {
        // Accept current Phase-3 messages, but allow early DECIDE from any round.
        let stamp = round.stamp();
        let round_id = self.round_of(&stamp);
        let phase_tag = self.phase_of(&stamp);
        let filter = round
            .filter()
            .level_cmp(0, TagCmp::Eq)
            .or_pattern(|p| {
                p.level_cmp(0, TagCmp::Gte)
                    .level_eq_value(1, Phase::Fourth.tag())
            });

        let mut out = Vec::new();
        for msg in collect(round, &filter, min, max) {
            let msg_stamp = msg.round_stamp();
            if self.phase_of(&msg_stamp) == Phase::Fourth.tag() {
                return CollectOutcome::Decide(msg);
            }
            if self.round_of(&msg_stamp) == round_id && self.phase_of(&msg_stamp) == phase_tag {
                match msg.payload(round) {
                    Message::Third(payload) => out.push(payload.clone()),
                    _ => panic!("expected ThirdPhaseMsg"),
                }
            }
        }
        CollectOutcome::Messages(out)
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
            .filter_map(|msg| match msg.payload(round) {
                Message::Decide(payload) => Some(payload.clone()),
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
        messages.iter().all(|m| m.ack)
    }
}

fn start_node(collect: &RoundCollector, scheme: RoundScheme, num_rounds: u32) -> Vec<LogEntry> {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, scheme, num_rounds).run(collect)
}

fn assert_agreement(logs: &[Vec<LogEntry>]) {
    let mut chosen: Option<bool> = None;

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

fn run_protocol(
    collect: Arc<RoundCollector>,
    num_nodes: usize,
    num_rounds: u32,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        // Tag layout (outer -> inner): (round, phase).
        // - round: grows monotonically, so we use Gte by default;
        // - phase: bounded step tag (First..Fourth), so we use Eq by default.
        let scheme = RoundScheme::builder()
            .level("round", TagCmp::Gte)
            .level("phase", TagCmp::Eq)
            .build();

        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            let receive = collect.clone();
            let scheme = scheme.clone();
            handles.push(thread::spawn(move || start_node(receive.as_ref(), scheme, num_rounds)));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(nodes.clone()));
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }
        assert_agreement(&logs);
    })
}

fn run_protocol_with_recv() -> traceforge::Stats {
    run_protocol_with_recv_params(DEFAULT_NUM_NODES, DEFAULT_NUM_ROUNDS)
}

fn run_protocol_with_recv_params(num_nodes: usize, num_rounds: u32) -> traceforge::Stats {
    let collect: Arc<RoundCollector> = Arc::new(move |round, filter, min, max| {
        let upper = match max {
            Some(upper) => upper,
            None => (num_rounds as usize) * 4 * num_nodes,
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
            out.push(msg);
        }
        out
    });
    run_protocol(collect, num_nodes, num_rounds)
}

fn run_protocol_with_inbox() -> traceforge::Stats {
    run_protocol_with_inbox_params(DEFAULT_NUM_NODES, DEFAULT_NUM_ROUNDS)
}

fn run_protocol_with_inbox_params(num_nodes: usize, num_rounds: u32) -> traceforge::Stats {
    let collect: Arc<RoundCollector> = Arc::new(move |round, filter, min, max| {
        comm_close::inbox_with_bounds_filter(filter, min, max)
            .into_iter()
            .flatten()
            .filter_map(|msg| {
                msg.payload(round)
                    .as_any_ref()
                    .downcast_ref::<Message>()
                    .cloned()
                    .map(|payload| comm_close::RoundMsg::from_parts(msg.round_id(), payload))
            })
            .collect()
    });
    run_protocol(collect, num_nodes, num_rounds)
}

fn parse_args() -> (usize, u32, bool, bool) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_rounds = DEFAULT_NUM_ROUNDS;
    let mut use_recv = false;
    let mut use_inbox = false;
    let mut args = std::env::args().skip(1).peekable();

    while let Some(arg) = args.next() {
        if arg == "recv" {
            use_recv = true;
        } else if arg == "inbox" {
            use_inbox = true;
        } else if arg == "--nodes" || arg == "--node" {
            let value = args
                .next()
                .unwrap_or_else(|| panic!("{} requires a value", arg));
            num_nodes = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid {} value: {}", arg, value));
        } else if arg == "--rounds" {
            let value = args
                .next()
                .unwrap_or_else(|| panic!("--rounds requires a value"));
            num_rounds = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid --rounds value: {}", value));
        } else {
            panic!("unknown argument: {}", arg);
        }
    }

    (num_nodes, num_rounds, use_recv, use_inbox)
}

fn main() {
    let (num_nodes, num_rounds, use_recv, use_inbox) = parse_args();

    if (use_recv && use_inbox) {
        panic!("Can't use recv/inbox at the same time!");
    } else if !use_recv && !use_inbox {
        panic!("Must specify recv or inbox!");
    }

    let stats = if use_recv {
        run_protocol_with_recv_params(num_nodes, num_rounds)
    } else {
        run_protocol_with_inbox_params(num_nodes, num_rounds)
    };
    println!("Stats = {}, {}", stats.execs, stats.block);
}
