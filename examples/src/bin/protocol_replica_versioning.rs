use std::collections::HashSet;

use traceforge::comm_close::{self, TraceForgeTransportMode};
use traceforge::thread;
use traceforge::thread::ThreadId;
use traceforge::{BranchingStrategy, Nondet};
use traceforge_rounds::{Comm, Round};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_VERSIONS: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Inbox;
const DEFAULT_USE_TAGS: bool = true;
const MAX_VALUE: u32 = 2;
const INIT_TAG: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
struct ReplicaRound {
    version: u32,
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
enum Message {
    Init(Participants),
    Update { version: u32, value: u32 },
    Ack { version: u32, sender: ThreadId },
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    writer: ThreadId,
    comm: Comm<ReplicaRound, comm_close::TraceForgeTransport>,
    mode: ReceiveMode,
    num_versions: u32,
    committed: Vec<Option<u32>>,
}

impl Node {
    fn new(nodes: Participants, num_versions: u32, mode: ReceiveMode, use_tags: bool) -> Self {
        let me = thread::current_id();
        let writer = *nodes
            .nodes
            .first()
            .expect("participants must contain at least one node");
        let comm = comm_close::comm_with::<ReplicaRound>(transport_mode(mode, use_tags));
        Self {
            nodes,
            me,
            writer,
            comm,
            mode,
            num_versions,
            committed: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<Option<u32>> {
        if self.me == self.writer {
            self.run_writer();
        } else {
            self.run_replica();
        }
        self.committed
    }

    fn run_writer(&mut self) {
        let quorum = self.nodes.len() / 2 + 1;
        let needed_from_replicas = quorum.saturating_sub(1);

        for _ in 0..self.num_versions {
            let round = self.comm.rounds().advance(ReplicaRound::dim_version());
            let version = round.version();
            let value = (0..=MAX_VALUE as usize).nondet() as u32;

            self.broadcast(Message::Update { version, value });
            let ack_senders = self.collect_quorum_acks(needed_from_replicas);
            if ack_senders.len() >= needed_from_replicas {
                self.commit_version(version, value);
            }
        }
    }

    fn run_replica(&mut self) {
        for _ in 0..self.num_versions {
            if let Some((version, value)) = self.receive_update() {
                if version > self.comm.rounds().current().version() {
                    self.comm.rounds().advance_to_version(version).unwrap();
                }
                self.commit_version(version, value);
                self.comm
                    .send(
                        self.writer,
                        Message::Ack {
                            version,
                            sender: self.me,
                        },
                    )
                    .unwrap();
            }
        }
    }

    fn broadcast(&mut self, msg: Message) {
        for node in self.nodes.iter() {
            if *node == self.me {
                continue;
            }
            self.comm.send(*node, msg.clone()).unwrap();
        }
    }

    fn receive_update(&mut self) -> Option<(u32, u32)> {
        match self.mode {
            ReceiveMode::Recv => {
                let count = (0..=1).nondet();
                if count == 0 {
                    return None;
                }
                let msg = self.comm.recv_block::<Message>().unwrap();
                if let Message::Update { version, value } = msg {
                    return Some((version, value));
                }
                None
            }
            ReceiveMode::Inbox => {
                let msgs = self.comm.inbox_with_bounds::<Message>(0, Some(1)).unwrap();
                for msg in msgs.into_iter().flatten() {
                    if let Message::Update { version, value } = msg {
                        return Some((version, value));
                    }
                }
                None
            }
        }
    }

    fn collect_quorum_acks(&mut self, needed: usize) -> HashSet<ThreadId> {
        if needed == 0 {
            return HashSet::new();
        }
        let max_expected = self.nodes.len().saturating_sub(1);

        let mut senders = HashSet::new();
        match self.mode {
            ReceiveMode::Recv => {
                let count = if max_expected == 0 {
                    0
                } else {
                    (0..=max_expected).nondet()
                };
                for _ in 0..count {
                    let msg = self
                        .comm
                        .recv_block_with::<Message, _>(same_version_filter)
                        .unwrap();
                    if let Message::Ack { sender, .. } = msg {
                        senders.insert(sender);
                    }
                }
            }
            ReceiveMode::Inbox => {
                let msgs = self
                    .comm
                    .inbox_with_bounds_with::<Message, _>(
                        0,
                        Some(max_expected),
                        same_version_filter,
                    )
                    .unwrap();
                for msg in msgs.into_iter().flatten() {
                    if let Message::Ack { sender, .. } = msg {
                        senders.insert(sender);
                    }
                }
            }
        }
        senders
    }

    fn commit_version(&mut self, version: u32, value: u32) {
        let idx = version as usize;
        while self.committed.len() <= idx {
            self.committed.push(None);
        }
        match self.committed[idx] {
            Some(previous) => assert_eq!(previous, value),
            None => {
                self.committed[idx] = Some(value);
            }
        }
    }
}

fn same_version_filter(local: &ReplicaRound, remote: &ReplicaRound) -> bool {
    local.version() == remote.version()
}

fn start_node(num_versions: u32, mode: ReceiveMode, use_tags: bool) -> Vec<Option<u32>> {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, num_versions, mode, use_tags).run()
}

fn assert_committed_consistency(logs: &[Vec<Option<u32>>]) {
    let max_len = logs.iter().map(|log| log.len()).max().unwrap_or(0);
    for idx in 0..max_len {
        let mut chosen: Option<u32> = None;
        for log in logs {
            if let Some(Some(value)) = log.get(idx) {
                if let Some(prev) = chosen {
                    assert_eq!(prev, *value);
                } else {
                    chosen = Some(*value);
                }
            }
        }
    }
}

fn run_protocol(
    num_nodes: usize,
    num_versions: u32,
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
                start_node(num_versions, mode, use_tags)
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
        assert_committed_consistency(&logs);
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
    let mut num_versions = DEFAULT_NUM_VERSIONS;
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
            "--versions" => {
                let value = next_arg_value(&mut args, "--versions");
                num_versions = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --versions value: {value}"));
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
                    "unknown argument: {arg} (expected --nodes <n>, --versions <n>, --mode <recv|inbox>, --wo-tags, --parallel <n>, --rayon <n>)",
                );
            }
        }
    }

    (num_nodes, num_versions, mode, use_tags, parallel)
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
    let (num_nodes, num_versions, mode, use_tags, parallel) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, num_versions, mode, use_tags, parallel);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
