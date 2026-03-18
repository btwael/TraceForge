use traceforge::new_comm_close::{MatchKind, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_PHASES: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Inbox;
const DEFAULT_USE_TAGS: bool = true;
const INIT_TAG: u32 = 1;
const MAX_VALUE: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum Step {
    Collect,
    Candidate,
    Quorum,
    Accept,
}

#[derive(Clone, traceforge::Round)]
struct LastVotingRound {
    #[dimension("=")]
    phase: u32,
    #[dimension("=")]
    step: Step,
}

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CollectMsg {
    phase: u32,
    sender: ThreadId,
    value: u32,
    ts: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CandidateMsg {
    phase: u32,
    sender: ThreadId,
    vote: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QuorumMsg {
    phase: u32,
    sender: ThreadId,
    value: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptMsg {
    phase: u32,
    sender: ThreadId,
    decision: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    Collect(CollectMsg),
    Candidate(CandidateMsg),
    Quorum(QuorumMsg),
    Accept(AcceptMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogEntry {
    phase: u32,
    coordinator: ThreadId,
    decision: u32,
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds<LastVotingRound>,
    mode: ReceiveMode,
    num_phases: u32,
    started: bool,

    x: u32,
    ts: Option<u32>,
    vote: u32,
    commit: bool,
    ready: bool,
    decided: Option<u32>,
}

impl Node {
    fn new(nodes: Participants, num_phases: u32, mode: ReceiveMode, use_tags: bool) -> Self {
        let me = thread::current().id();
        let x = (0..=MAX_VALUE as usize).nondet() as u32;
        let rounds = if use_tags {
            Rounds::<LastVotingRound>::new()
        } else {
            Rounds::<LastVotingRound>::new_wo_tags(false)
        };
        Self {
            nodes,
            me,
            rounds,
            mode,
            num_phases,
            started: false,
            x,
            ts: None,
            vote: x,
            commit: false,
            ready: false,
            decided: None,
        }
    }

    fn run(mut self) -> Vec<LogEntry> {
        let mut log = Vec::new();
        for _ in 0..self.num_phases {
            if self.decided.is_some() {
                break;
            }
            self.step_phase(&mut log);
        }
        log
    }

    fn step_phase(&mut self, log: &mut Vec<LogEntry>) {
        let phase = self.enter_next_phase().phase();
        let coordinator = self.coordinator(phase);
        let quorum = self.nodes.len() / 2;

        // Round 1: Collect
        self.rounds.send(
            coordinator,
            Message::Collect(CollectMsg {
                phase,
                sender: self.me,
                value: self.x,
                ts: self.ts,
            }),
        );
        let collect = self.collect_step_messages(self.nodes.len());
        if self.me == coordinator {
            let collected: Vec<CollectMsg> = collect
                .into_iter()
                .filter_map(|msg| match msg {
                    Message::Collect(m) if m.phase == phase => Some(m),
                    _ => None,
                })
                .collect();
            if collected.len() > quorum {
                self.vote = Self::select_max_ts_value(&collected);
                self.commit = true;
            }
        }

        // Round 2: Candidate
        self.rounds.advance(LastVotingRound::step());
        if self.me == coordinator && self.commit {
            self.broadcast(Message::Candidate(CandidateMsg {
                phase,
                sender: self.me,
                vote: self.vote,
            }));
        }
        let candidate = self.collect_step_messages(self.nodes.len());
        for msg in candidate {
            if let Message::Candidate(m) = msg {
                if m.phase == phase && m.sender == coordinator {
                    self.x = m.vote;
                    self.ts = Some(phase);
                    break;
                }
            }
        }

        // Round 3: Quorum
        self.rounds.advance(LastVotingRound::step());
        if self.ts == Some(phase) {
            self.rounds.send(
                coordinator,
                Message::Quorum(QuorumMsg {
                    phase,
                    sender: self.me,
                    value: self.x,
                }),
            );
        }
        let quorum_msgs = self.collect_step_messages(self.nodes.len());
        if self.me == coordinator {
            let votes = quorum_msgs
                .into_iter()
                .filter(|msg| matches!(msg, Message::Quorum(m) if m.phase == phase))
                .count();
            if votes > quorum {
                self.ready = true;
            }
        }

        // Round 4: Accept
        self.rounds.advance(LastVotingRound::step());
        if self.me == coordinator && self.ready {
            self.broadcast(Message::Accept(AcceptMsg {
                phase,
                sender: self.me,
                decision: self.vote,
            }));
        }
        let accepts = self.collect_step_messages(self.nodes.len());
        if self.decided.is_none() {
            for msg in accepts {
                if let Message::Accept(m) = msg {
                    if m.phase == phase && m.sender == coordinator {
                        self.decided = Some(m.decision);
                        log.push(LogEntry {
                            phase,
                            coordinator,
                            decision: m.decision,
                        });
                        self.ready = false;
                        self.commit = false;
                        break;
                    }
                }
            }
        }
    }

    fn collect_step_messages(&self, max: usize) -> Vec<Message> {
        let filter = self
            .rounds
            .filter()
            .phase(MatchKind::Eq)
            .step(MatchKind::Eq);
        match self.mode {
            ReceiveMode::Recv => {
                let count = (0..=max).nondet();
                let mut out = Vec::new();
                for _ in 0..count {
                    let msg = self.rounds.recv_block_with::<Message>(&filter);
                    out.push(msg.payload().clone());
                }
                out
            }
            ReceiveMode::Inbox => self
                .rounds
                .inbox_with_bounds_with::<Message>(&filter, 0, Some(max))
                .into_iter()
                .flatten()
                .map(|msg| msg.payload().clone())
                .collect(),
        }
    }

    fn enter_next_phase(&mut self) -> traceforge::new_comm_close::Round<LastVotingRound> {
        if self.started {
            self.rounds.advance(LastVotingRound::phase())
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn coordinator(&self, phase: u32) -> ThreadId {
        let idx = phase as usize % self.nodes.len();
        self.nodes.nodes[idx]
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            self.rounds.send(*node, msg.clone());
        }
    }

    fn select_max_ts_value(messages: &[CollectMsg]) -> u32 {
        let mut best_ts: Option<u32> = None;
        let mut best_value = messages[0].value;
        for msg in messages {
            let better = match (msg.ts, best_ts) {
                (Some(a), Some(b)) => a > b,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if better {
                best_ts = msg.ts;
                best_value = msg.value;
            }
        }
        best_value
    }
}

fn start_node(num_phases: u32, mode: ReceiveMode, use_tags: bool) -> Vec<LogEntry> {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, num_phases, mode, use_tags).run()
}

fn assert_decision_consistency(logs: &[Vec<LogEntry>]) {
    let mut decided: Option<u32> = None;
    for log in logs {
        for entry in log {
            if let Some(prev) = decided {
                assert_eq!(prev, entry.decision);
            } else {
                decided = Some(entry.decision);
            }
        }
    }
}

fn run_protocol(
    num_nodes: usize,
    num_phases: u32,
    mode: ReceiveMode,
    use_tags: bool,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            handles.push(thread::spawn(move || start_node(num_phases, mode, use_tags)));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            if use_tags {
                traceforge::send_msg(handle.thread().id(), Message::Init(nodes.clone()));
            } else {
                traceforge::send_tagged_msg(handle.thread().id(), INIT_TAG, Message::Init(nodes.clone()));
            }
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }
        assert_decision_consistency(&logs);
    })
}

fn parse_mode(value: &str) -> ReceiveMode {
    match value {
        "recv" => ReceiveMode::Recv,
        "inbox" => ReceiveMode::Inbox,
        _ => panic!("invalid --mode value: {} (expected recv or inbox)", value),
    }
}

fn parse_args() -> (usize, u32, ReceiveMode, bool) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_phases = DEFAULT_NUM_PHASES;
    let mut mode = DEFAULT_MODE;
    let mut use_tags = DEFAULT_USE_TAGS;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nodes" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| panic!("--nodes requires a value"));
                num_nodes = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --nodes value: {}", value));
            }
            "--phases" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| panic!("--phases requires a value"));
                num_phases = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --phases value: {}", value));
            }
            "--mode" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| panic!("--mode requires a value"));
                mode = parse_mode(&value);
            }
            "--wo-tags" => {
                use_tags = false;
            }
            _ => {
                panic!(
                    "unknown argument: {} (expected --nodes <n>, --phases <n>, --mode <recv|inbox>, --wo-tags)",
                    arg
                );
            }
        }
    }

    (num_nodes, num_phases, mode, use_tags)
}

fn main() {
    let (num_nodes, num_phases, mode, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, num_phases, mode, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
