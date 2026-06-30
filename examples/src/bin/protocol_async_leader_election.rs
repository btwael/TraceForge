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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
enum Phase {
    NewBallot,
    AckBallot,
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
struct LeaderRound {
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
struct NewBallotMsg {
    leader: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AckBallotMsg {
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

struct Node {
    nodes: Participants,
    me: ThreadId,
    comm: Comm<LeaderRound, comm_close::TraceForgeTransport>,
    leader: ThreadId,
    started: bool,
    num_ballots: u32,
    mode: ReceiveMode,
}

impl Node {
    fn new(nodes: Participants, num_ballots: u32, mode: ReceiveMode, use_tags: bool) -> Self {
        let me = thread::current_id();
        let comm = comm_close::comm_with::<LeaderRound>(transport_mode(mode, use_tags));
        Self {
            nodes,
            me,
            comm,
            leader: me,
            started: false,
            num_ballots,
            mode,
        }
    }

    fn run(mut self) -> Vec<LogEntry> {
        let mut log = Vec::new();
        for _ in 0..self.num_ballots {
            self.step(&mut log);
        }
        log
    }

    fn step(&mut self, log: &mut Vec<LogEntry>) {
        self.next_round();
        if self.coord() {
            self.run_leader_round(log);
        } else {
            self.run_follower_round(log);
        }
    }

    fn run_leader_round(&mut self, log: &mut Vec<LogEntry>) {
        self.phase_new_ballot_as_leader();
        self.phase_ack_ballot(log);
    }

    fn phase_new_ballot_as_leader(&mut self) {
        self.broadcast(Message::NewBallot(NewBallotMsg {
            leader: self.me,
            sender: self.me,
        }));
        self.leader = self.me;
        self.comm.rounds().advance(LeaderRound::dim_phase());
    }

    fn run_follower_round(&mut self, log: &mut Vec<LogEntry>) {
        let new_ballot_msgs = self.collect_new_ballot();
        if new_ballot_msgs.len() == 1 {
            let (stamp, msg) = &new_ballot_msgs[0];
            self.comm.rounds().jump(stamp.clone()).unwrap();
            match msg {
                Message::NewBallot(payload) => self.leader = payload.leader,
                Message::AckBallot(payload) => self.leader = payload.leader,
                _ => panic!("expected NewBallot or AckBallot"),
            }

            self.enter_ack_ballot_phase_if_needed();
            self.phase_ack_ballot(log);
        }
    }

    fn enter_ack_ballot_phase_if_needed(&mut self) {
        if self.comm.rounds().current().phase() == Phase::NewBallot {
            self.comm.rounds().advance(LeaderRound::dim_phase());
        }
    }

    fn phase_ack_ballot(&mut self, log: &mut Vec<LogEntry>) {
        let ballot = self.comm.rounds().current().ballot();
        self.broadcast(Message::AckBallot(AckBallotMsg {
            leader: self.leader,
            sender: self.me,
        }));

        let recv_max = self.nodes.len() - 1;
        let quorum = self.nodes.len() / 2;
        let ack_msgs = self.collect_ack_ballot(recv_max);
        if ack_msgs.len() > quorum && Self::all_same_leader(&ack_msgs, self.leader) {
            log.push(LogEntry {
                ballot,
                leader: self.leader,
            });
        }
    }

    fn next_round(&mut self) -> &LeaderRound {
        if self.started {
            self.comm.rounds().advance(LeaderRound::dim_ballot())
        } else {
            self.started = true;
            self.comm.rounds().current()
        }
    }

    fn coord(&self) -> bool {
        traceforge::nondet()
    }

    fn broadcast(&mut self, msg: Message) {
        for node in self.nodes.iter() {
            self.comm.send(*node, msg.clone()).unwrap();
        }
    }

    fn collect_new_ballot(&mut self) -> Vec<(LeaderRound, Message)> {
        self.collect_messages(self.mode, None, 0, 1)
    }

    fn collect_ack_ballot(&mut self, max: usize) -> Vec<(AckBallotMsg, LeaderRound)> {
        self.collect_messages(self.mode, Some(ack_filter), 0, max)
            .into_iter()
            .map(|(stamp, msg)| match msg {
                Message::AckBallot(payload) => (payload, stamp),
                _ => panic!("expected AckBallotMsg"),
            })
            .collect()
    }

    fn collect_messages(
        &mut self,
        mode: ReceiveMode,
        filter: Option<fn(&LeaderRound, &LeaderRound) -> bool>,
        min: usize,
        max: usize,
    ) -> Vec<(LeaderRound, Message)> {
        assert!(max >= min, "requires max >= min");
        match mode {
            ReceiveMode::Recv => {
                let mut out = Vec::new();
                let count = if max == min {
                    min
                } else {
                    (min..=max).nondet()
                };
                for _ in 0..count {
                    let msg = match filter {
                        Some(filter) => self
                            .comm
                            .recv_block_stamped_with::<Message, _>(filter)
                            .unwrap(),
                        None => self.comm.recv_block_stamped::<Message>().unwrap(),
                    };
                    out.push(msg);
                }
                out
            }
            ReceiveMode::Inbox => {
                let msgs = match filter {
                    Some(filter) => self
                        .comm
                        .inbox_stamped_with_bounds_with::<Message, _>(min, Some(max), filter)
                        .unwrap(),
                    None => self
                        .comm
                        .inbox_stamped_with_bounds::<Message>(min, Some(max))
                        .unwrap(),
                };
                msgs.into_iter().flatten().collect()
            }
        }
    }

    fn all_same_leader(messages: &[(AckBallotMsg, LeaderRound)], leader: ThreadId) -> bool {
        messages.iter().all(|(msg, _)| msg.leader == leader)
    }
}

fn ack_filter(local: &LeaderRound, remote: &LeaderRound) -> bool {
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

fn assert_log_consistency(logs: &[Vec<LogEntry>], num_ballots: u32) {
    for ballot in 0..=num_ballots {
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
                .with_partitioned_branching(BranchingStrategy::RevisitQueueRayon);
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
        assert_log_consistency(&logs, num_ballots);
    })
}

fn transport_mode(mode: ReceiveMode, use_tags: bool) -> TraceForgeTransportMode {
    match (mode, use_tags) {
        (ReceiveMode::Inbox, true) => TraceForgeTransportMode::TaggedNativeInbox,
        (ReceiveMode::Recv, true) => TraceForgeTransportMode::TaggedRepeatedRecv,
        (ReceiveMode::Recv, false) => TraceForgeTransportMode::UntaggedRepeatedRecv,
        (ReceiveMode::Inbox, false) => {
            panic!("--wo-tags is only supported with --mode recv");
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
                mode = parse_mode(&value);
            }
            "--wo-tags" => {
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
                    "unknown argument: {arg} (expected --nodes <n>, --ballots <n>, --mode <recv|inbox>, --wo-tags, --parallel <n>, --rayon <n>)",
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

fn parse_mode(value: &str) -> ReceiveMode {
    match value {
        "recv" => ReceiveMode::Recv,
        "inbox" => ReceiveMode::Inbox,
        _ => panic!("invalid --mode value: {value} (expected recv or inbox)"),
    }
}

fn main() {
    let (num_nodes, num_ballots, mode, use_tags, parallel) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, num_ballots, mode, use_tags, parallel);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
