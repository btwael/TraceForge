use traceforge::comm_close::{self, TraceForgeTransportMode};
use traceforge::thread;
use traceforge::thread::ThreadId;
use traceforge::{BranchingStrategy, Nondet};
use traceforge_rounds::{Comm, Dim, Round};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
enum Step {
    Collect,
    Candidate,
    Quorum,
    Accept,
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
struct LastVotingRound {
    phase: u32,
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
    comm: Comm<LastVotingRound, comm_close::TraceForgeTransport>,
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
        let me = thread::current_id();
        let x = (0..=MAX_VALUE as usize).nondet() as u32;
        let comm = comm_close::comm_with::<LastVotingRound>(transport_mode(mode, use_tags));
        Self {
            nodes,
            me,
            comm,
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

        self.comm
            .send(
                coordinator,
                Message::Collect(CollectMsg {
                    phase,
                    sender: self.me,
                    value: self.x,
                    ts: self.ts,
                }),
            )
            .unwrap();
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

        self.comm.rounds().advance(LastVotingRound::dim_step());
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

        self.comm.rounds().advance(LastVotingRound::dim_step());
        if self.ts == Some(phase) {
            self.comm
                .send(
                    coordinator,
                    Message::Quorum(QuorumMsg {
                        phase,
                        sender: self.me,
                        value: self.x,
                    }),
                )
                .unwrap();
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

        self.comm.rounds().advance(LastVotingRound::dim_step());
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

    fn collect_step_messages(&mut self, max: usize) -> Vec<Message> {
        match self.mode {
            ReceiveMode::Recv => {
                let count = (0..=max).nondet();
                let mut out = Vec::new();
                for _ in 0..count {
                    let msg = self
                        .comm
                        .recv_block_with::<Message, _>(same_step_filter)
                        .unwrap();
                    out.push(msg);
                }
                out
            }
            ReceiveMode::Inbox => self
                .comm
                .inbox_with_bounds_with::<Message, _>(0, Some(max), same_step_filter)
                .unwrap()
                .into_iter()
                .flatten()
                .collect(),
        }
    }

    fn enter_next_phase(&mut self) -> &LastVotingRound {
        if self.started {
            self.comm.rounds().advance(LastVotingRound::dim_phase())
        } else {
            self.started = true;
            self.comm.rounds().current()
        }
    }

    fn coordinator(&self, phase: u32) -> ThreadId {
        let idx = phase as usize % self.nodes.len();
        self.nodes.nodes[idx]
    }

    fn broadcast(&mut self, msg: Message) {
        for node in self.nodes.iter() {
            self.comm.send(*node, msg.clone()).unwrap();
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

fn same_step_filter(local: &LastVotingRound, remote: &LastVotingRound) -> bool {
    local.phase() == remote.phase() && local.step() == remote.step()
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
    parallel: ParallelMode,
) -> traceforge::Stats {
    let mut config = traceforge::Config::builder();
    match parallel {
        ParallelMode::Sequential => {}
        ParallelMode::Shared(workers) => {
            config = config.with_parallel(true).with_parallel_workers(workers);
        }
        ParallelMode::Rayon(workers) => {
            config = config
                .with_partitioned_parallelization(true)
                .with_partitioned_num_threads(workers)
                .with_partitioned_branching(BranchingStrategy::RevisitQueueRayon)
                .with_iterations_until_split(5000);
        }
    }

    traceforge::verify(config.build(), move || {
        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            handles.push(thread::spawn(move || {
                start_node(num_phases, mode, use_tags)
            }));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            if use_tags {
                traceforge::send_msg(handle.thread().id(), Message::Init(nodes.clone()));
            } else {
                traceforge::send_tagged_msg(
                    handle.thread().id(),
                    INIT_TAG,
                    Message::Init(nodes.clone()),
                );
            }
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }
        assert_decision_consistency(&logs);
    })
}

fn transport_mode(mode: ReceiveMode, use_tags: bool) -> TraceForgeTransportMode {
    match (mode, use_tags) {
        (ReceiveMode::Inbox, true) => TraceForgeTransportMode::TaggedNativeInbox,
        (ReceiveMode::Recv, true) => TraceForgeTransportMode::TaggedRepeatedRecv,
        (ReceiveMode::Recv, false) => TraceForgeTransportMode::UntaggedRepeatedRecv,
        (ReceiveMode::Inbox, false) => {
            panic!("--wo-tags is only supported with --mode rounds/recv");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParallelMode {
    Sequential,
    Shared(usize),
    Rayon(usize),
}

fn parse_args() -> (usize, u32, ReceiveMode, bool, ParallelMode) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_phases = DEFAULT_NUM_PHASES;
    let mut mode = DEFAULT_MODE;
    let mut use_tags = DEFAULT_USE_TAGS;
    let mut explicit_wo_tags = false;
    let mut parallel = ParallelMode::Sequential;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nodes" => {
                let value = next_arg_value(&mut args, "--nodes");
                num_nodes = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --nodes value: {value}"));
            }
            "--phases" => {
                let value = next_arg_value(&mut args, "--phases");
                num_phases = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --phases value: {value}"));
            }
            "--mode" => {
                let value = next_arg_value(&mut args, "--mode");
                let parsed = parse_mode(&value);
                mode = parsed.0;
                use_tags = parsed.1 && !explicit_wo_tags;
            }
            "--wo-tags" => {
                explicit_wo_tags = true;
                use_tags = false;
            }
            "--parallel" => {
                let workers = parse_workers(&mut args, "--parallel");
                if parallel != ParallelMode::Sequential {
                    panic!("only one parallel mode can be selected");
                }
                parallel = ParallelMode::Shared(workers);
            }
            "--rayon" => {
                let workers = parse_workers(&mut args, "--rayon");
                if parallel != ParallelMode::Sequential {
                    panic!("only one parallel mode can be selected");
                }
                parallel = ParallelMode::Rayon(workers);
            }
            _ => {
                panic!(
                    "unknown argument: {arg} (expected --nodes <n>, --phases <n>, --mode <full|rounds|dpor>, --wo-tags, --parallel <n>, --rayon <n>)",
                );
            }
        }
    }

    (num_nodes, num_phases, mode, use_tags, parallel)
}

fn parse_workers(args: &mut impl Iterator<Item = String>, flag: &str) -> usize {
    let value = next_arg_value(args, flag);
    let workers = value
        .parse()
        .unwrap_or_else(|_| panic!("invalid {flag} value: {value}"));
    if workers == 0 {
        panic!("{flag} requires a positive worker count");
    }
    workers
}

fn next_arg_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
}

fn parse_mode(value: &str) -> (ReceiveMode, bool) {
    match value {
        "full" | "inbox" => (ReceiveMode::Inbox, true),
        "rounds" | "recv" => (ReceiveMode::Recv, true),
        "dpor" => (ReceiveMode::Recv, false),
        _ => panic!("invalid --mode value: {value} (expected full, rounds, dpor, inbox, or recv)"),
    }
}

fn main() {
    let (num_nodes, num_phases, mode, use_tags, parallel) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode rounds/recv");
    }
    let stats = run_protocol(num_nodes, num_phases, mode, use_tags, parallel);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
