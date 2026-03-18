use std::collections::HashSet;

use traceforge::comm_close::{MatchKind, RoundFilter, Rounds};
use traceforge::thread::ThreadId;
use traceforge::thread;
use traceforge::Nondet;

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

#[derive(Clone, traceforge::Round)]
struct ReplicaRound {
    #[dimension(">=")]
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
    rounds: Rounds<ReplicaRound>,
    mode: ReceiveMode,
    num_versions: u32,
    committed: Vec<Option<u32>>,
}

impl Node {
    fn new(nodes: Participants, num_versions: u32, mode: ReceiveMode, use_tags: bool) -> Self {
        let me = thread::current().id();
        let writer = *nodes
            .nodes
            .first()
            .expect("participants must contain at least one node");
        let rounds = if use_tags {
            Rounds::<ReplicaRound>::new()
        } else {
            Rounds::<ReplicaRound>::new_wo_tags(false)
        };
        Self {
            nodes,
            me,
            writer,
            rounds,
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
            let round = self.rounds.advance(ReplicaRound::version());
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
                if version > self.rounds.current().version() {
                    self.rounds.advance_to(ReplicaRound::version(), version);
                }
                self.commit_version(version, value);
                self.rounds.send(
                    self.writer,
                    Message::Ack {
                        version,
                        sender: self.me,
                    },
                );
            }
        }
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            if *node == self.me {
                continue;
            }
            self.rounds.send(*node, msg.clone());
        }
    }

    fn receive_update(&self) -> Option<(u32, u32)> {
        let filter: RoundFilter<ReplicaRound> = self
            .rounds
            .filter()
            .version(MatchKind::Gte);

        match self.mode {
            ReceiveMode::Recv => {
                let count = (0..=1).nondet();
                if count == 0 {
                    return None;
                }
                let msg = self.rounds.recv_block_with::<Message>(&filter);
                if let Message::Update { version, value } = msg.payload() {
                    return Some((*version, *value));
                }
                None
            }
            ReceiveMode::Inbox => {
                let msgs = self
                    .rounds
                    .inbox_with_bounds_with::<Message>(&filter, 0, Some(1));
                for msg in msgs.into_iter().flatten() {
                    if let Message::Update { version, value } = msg.payload() {
                        return Some((*version, *value));
                    }
                }
                None
            }
        }
    }

    fn collect_quorum_acks(&self, needed: usize) -> HashSet<ThreadId> {
        if needed == 0 {
            return HashSet::new();
        }
        let max_expected = self.nodes.len().saturating_sub(1);

        let filter: RoundFilter<ReplicaRound> = self
            .rounds
            .filter()
            .version(MatchKind::Eq);

        let mut senders = HashSet::new();
        match self.mode {
            ReceiveMode::Recv => {
                let count = if max_expected == 0 {
                    0
                } else {
                    (0..=max_expected).nondet()
                };
                for _ in 0..count {
                    let msg = self.rounds.recv_block_with::<Message>(&filter);
                    if let Message::Ack { sender, .. } = msg.payload() {
                        senders.insert(*sender);
                    }
                }
            }
            ReceiveMode::Inbox => {
                let msgs = self
                    .rounds
                    .inbox_with_bounds_with::<Message>(&filter, 0, Some(max_expected));
                for msg in msgs.into_iter().flatten() {
                    if let Message::Ack { sender, .. } = msg.payload() {
                        senders.insert(*sender);
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

fn run_protocol(num_nodes: usize, num_versions: u32, mode: ReceiveMode, use_tags: bool) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            handles.push(thread::spawn(move || start_node(num_versions, mode, use_tags)));
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
        assert_committed_consistency(&logs);
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
    let mut num_versions = DEFAULT_NUM_VERSIONS;
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
            "--versions" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| panic!("--versions requires a value"));
                num_versions = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --versions value: {}", value));
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
                    "unknown argument: {} (expected --nodes <n>, --versions <n>, --mode <recv|inbox>, --wo-tags)",
                    arg
                );
            }
        }
    }

    (num_nodes, num_versions, mode, use_tags)
}

fn main() {
    let (num_nodes, num_versions, mode, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, num_versions, mode, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
