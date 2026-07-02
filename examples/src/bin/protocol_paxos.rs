use traceforge::comm_close::{self, TraceForgeTransportMode};
use traceforge::thread;
use traceforge::thread::ThreadId;
use traceforge::{BranchingStrategy, Nondet};
use traceforge_rounds::{Comm, Dim, Round};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_BALLOTS: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Inbox;
const DEFAULT_USE_TAGS: bool = true;
const INIT_TAG: u32 = 1;
const MAX_VALUE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
enum Phase {
    Prepare,
    Promise,
    Accept,
    Accepted,
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
struct PaxosRound {
    ballot: u32,
    phase: Phase,
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
struct PrepareMsg {
    ballot: u32,
    proposer: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PromiseMsg {
    ballot: u32,
    proposer: ThreadId,
    sender: ThreadId,
    accepted_ballot: Option<u32>,
    accepted_value: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptReqMsg {
    ballot: u32,
    proposer: ThreadId,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptedMsg {
    ballot: u32,
    proposer: ThreadId,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    Prepare(PrepareMsg),
    Promise(PromiseMsg),
    AcceptReq(AcceptReqMsg),
    Accepted(AcceptedMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogEntry {
    ballot: u32,
    proposer: ThreadId,
    value: u32,
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    comm: Comm<PaxosRound, comm_close::TraceForgeTransport>,
    mode: ReceiveMode,
    num_ballots: u32,
    started: bool,

    promised_ballot: Option<u32>,
    accepted_ballot: Option<u32>,
    accepted_value: Option<u32>,
    decided_value: Option<u32>,
}

impl Node {
    fn new(nodes: Participants, num_ballots: u32, mode: ReceiveMode, use_tags: bool) -> Self {
        let me = thread::current_id();
        let comm = comm_close::comm_with::<PaxosRound>(transport_mode(mode, use_tags));
        Self {
            nodes,
            me,
            comm,
            mode,
            num_ballots,
            started: false,
            promised_ballot: None,
            accepted_ballot: None,
            accepted_value: None,
            decided_value: None,
        }
    }

    fn run(mut self) -> Vec<LogEntry> {
        let mut log = Vec::new();
        for _ in 0..self.num_ballots {
            self.step_ballot(&mut log);
        }
        log
    }

    fn step_ballot(&mut self, log: &mut Vec<LogEntry>) {
        let ballot = self.enter_next_ballot().ballot();
        let quorum = self.nodes.len() / 2;

        let i_propose = traceforge::nondet() && self.decided_value.is_none();
        if i_propose {
            self.broadcast(Message::Prepare(PrepareMsg {
                ballot,
                proposer: self.me,
                sender: self.me,
            }));
        }
        self.handle_prepare_messages();

        self.comm.rounds().advance(PaxosRound::dim_phase());
        let mut proposed_value = None;
        if i_propose {
            let promises = self.collect_promises_for(ballot, self.me, self.nodes.len());
            if promises.len() > quorum {
                let value = Self::choose_proposal_value(&promises);
                proposed_value = Some(value);
                self.broadcast(Message::AcceptReq(AcceptReqMsg {
                    ballot,
                    proposer: self.me,
                    value,
                    sender: self.me,
                }));
            }
        }

        self.comm.rounds().advance(PaxosRound::dim_phase());
        self.handle_accept_requests();

        self.comm.rounds().advance(PaxosRound::dim_phase());
        if let Some(value) = proposed_value {
            let accepted = self.collect_accepted_for(ballot, self.me, self.nodes.len());
            let matching = accepted.iter().filter(|msg| msg.value == value).count();
            if matching > quorum {
                if let Some(prev) = self.decided_value {
                    assert_eq!(prev, value);
                }
                self.decided_value = Some(value);
                log.push(LogEntry {
                    ballot,
                    proposer: self.me,
                    value,
                });
            }
        }
    }

    fn enter_next_ballot(&mut self) -> &PaxosRound {
        if self.started {
            self.comm.rounds().advance(PaxosRound::dim_ballot())
        } else {
            self.started = true;
            self.comm.rounds().current()
        }
    }

    fn handle_prepare_messages(&mut self) {
        let prepares = self.collect_phase_messages(self.nodes.len());
        for msg in prepares {
            let prepare = match msg {
                Message::Prepare(prepare) => prepare,
                _ => continue,
            };

            if self.is_promised(prepare.ballot) {
                self.promised_ballot = Some(prepare.ballot);
                self.comm
                    .send(
                        prepare.proposer,
                        Message::Promise(PromiseMsg {
                            ballot: prepare.ballot,
                            proposer: prepare.proposer,
                            sender: self.me,
                            accepted_ballot: self.accepted_ballot,
                            accepted_value: self.accepted_value,
                        }),
                    )
                    .unwrap();
            }
        }
    }

    fn handle_accept_requests(&mut self) {
        let reqs = self.collect_phase_messages(self.nodes.len());
        for msg in reqs {
            let req = match msg {
                Message::AcceptReq(req) => req,
                _ => continue,
            };

            if self.can_accept(req.ballot, req.value) {
                self.promised_ballot = Some(req.ballot);
                self.accepted_ballot = Some(req.ballot);
                self.accepted_value = Some(req.value);
                self.comm
                    .send(
                        req.proposer,
                        Message::Accepted(AcceptedMsg {
                            ballot: req.ballot,
                            proposer: req.proposer,
                            value: req.value,
                            sender: self.me,
                        }),
                    )
                    .unwrap();
            }
        }
    }

    fn collect_promises_for(
        &mut self,
        ballot: u32,
        proposer: ThreadId,
        max: usize,
    ) -> Vec<PromiseMsg> {
        self.collect_phase_messages(max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Promise(p) if p.ballot == ballot && p.proposer == proposer => Some(p),
                _ => None,
            })
            .collect()
    }

    fn collect_accepted_for(
        &mut self,
        ballot: u32,
        proposer: ThreadId,
        max: usize,
    ) -> Vec<AcceptedMsg> {
        self.collect_phase_messages(max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Accepted(a) if a.ballot == ballot && a.proposer == proposer => Some(a),
                _ => None,
            })
            .collect()
    }

    fn collect_phase_messages(&mut self, max: usize) -> Vec<Message> {
        match self.mode {
            ReceiveMode::Recv => {
                let count = (0..=max).nondet();
                let mut out = Vec::new();
                for _ in 0..count {
                    let msg = self
                        .comm
                        .recv_block_with::<Message, _>(same_round_filter)
                        .unwrap();
                    out.push(msg);
                }
                out
            }
            ReceiveMode::Inbox => self
                .comm
                .inbox_with_bounds_with::<Message, _>(0, Some(max), same_round_filter)
                .unwrap()
                .into_iter()
                .flatten()
                .collect(),
        }
    }

    fn broadcast(&mut self, msg: Message) {
        for node in self.nodes.iter() {
            self.comm.send(*node, msg.clone()).unwrap();
        }
    }

    fn is_promised(&self, ballot: u32) -> bool {
        match self.promised_ballot {
            Some(p) => ballot >= p,
            None => true,
        }
    }

    fn can_accept(&self, ballot: u32, value: u32) -> bool {
        if !self.is_promised(ballot) {
            return false;
        }
        if self.accepted_ballot == Some(ballot) {
            return self.accepted_value == Some(value);
        }
        true
    }

    fn choose_proposal_value(promises: &[PromiseMsg]) -> u32 {
        let mut best: Option<(u32, u32)> = None;
        for p in promises {
            if let (Some(b), Some(v)) = (p.accepted_ballot, p.accepted_value) {
                match best {
                    Some((best_b, _)) if b <= best_b => (),
                    _ => best = Some((b, v)),
                }
            }
        }
        match best {
            Some((_, v)) => v,
            None => (0..=MAX_VALUE as usize).nondet() as u32,
        }
    }
}

fn same_round_filter(local: &PaxosRound, remote: &PaxosRound) -> bool {
    local.ballot() == remote.ballot() && local.phase() == remote.phase()
}

fn start_node(num_ballots: u32, mode: ReceiveMode, use_tags: bool) -> Vec<LogEntry> {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, num_ballots, mode, use_tags).run()
}

fn assert_decision_consistency(logs: &[Vec<LogEntry>]) {
    let mut decided: Option<u32> = None;
    for log in logs {
        for entry in log {
            if let Some(prev) = decided {
                assert_eq!(prev, entry.value);
            } else {
                decided = Some(entry.value);
            }
        }
    }
}

fn run_protocol(
    num_nodes: usize,
    num_ballots: u32,
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
                start_node(num_ballots, mode, use_tags)
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
    let mut num_ballots = DEFAULT_NUM_BALLOTS;
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
            "--ballots" => {
                let value = next_arg_value(&mut args, "--ballots");
                num_ballots = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --ballots value: {value}"));
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
                    "unknown argument: {arg} (expected --nodes <n>, --ballots <n>, --mode <full|rounds|dpor>, --wo-tags, --parallel <n>, --rayon <n>)",
                );
            }
        }
    }

    (num_nodes, num_ballots, mode, use_tags, parallel)
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
    let (num_nodes, num_ballots, mode, use_tags, parallel) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode rounds/recv");
    }
    let stats = run_protocol(num_nodes, num_ballots, mode, use_tags, parallel);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
