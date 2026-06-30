use std::collections::{HashMap, HashSet};

use traceforge::comm_close::{self, TraceForgeTransportMode};
use traceforge::thread;
use traceforge::thread::ThreadId;
use traceforge::{BranchingStrategy, Nondet};
use traceforge_rounds::{Comm, Dim, Round};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_TERMS: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Inbox;
const DEFAULT_USE_TAGS: bool = true;
const INIT_TAG: u32 = 1;
const MAX_VALUE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Follower,
    Candidate,
    Leader,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
enum Phase {
    RequestVote,
    Vote,
    Append,
    Ack,
    Commit,
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
struct RaftRound {
    term: u32,
    phase: Phase,
}

fn phase_ord(phase: Phase) -> u32 {
    match phase {
        Phase::RequestVote => 0,
        Phase::Vote => 1,
        Phase::Append => 2,
        Phase::Ack => 3,
        Phase::Commit => 4,
    }
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

    fn majority(&self) -> usize {
        (self.nodes.len() / 2) + 1
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestVoteMsg {
    term: u32,
    candidate: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VoteMsg {
    term: u32,
    candidate: ThreadId,
    granted: bool,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AppendEntriesMsg {
    term: u32,
    leader: ThreadId,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AckMsg {
    term: u32,
    leader: ThreadId,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitMsg {
    term: u32,
    leader: ThreadId,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    RequestVote(RequestVoteMsg),
    Vote(VoteMsg),
    AppendEntries(AppendEntriesMsg),
    Ack(AckMsg),
    Commit(CommitMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeaderEntry {
    term: u32,
    leader: ThreadId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommitEntry {
    term: u32,
    value: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeLog {
    leaders: Vec<LeaderEntry>,
    commits: Vec<CommitEntry>,
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    comm: Comm<RaftRound, comm_close::TraceForgeTransport>,
    started: bool,
    num_terms: u32,
    mode: ReceiveMode,

    role: Role,
    voted_for: Option<ThreadId>,
    current_leader: Option<ThreadId>,
    current_value: Option<u32>,
}

impl Node {
    fn new(nodes: Participants, num_terms: u32, mode: ReceiveMode, use_tags: bool) -> Self {
        let me = thread::current_id();
        let comm = comm_close::comm_with::<RaftRound>(transport_mode(mode, use_tags));
        Self {
            nodes,
            me,
            comm,
            started: false,
            num_terms,
            mode,
            role: Role::Follower,
            voted_for: None,
            current_leader: None,
            current_value: None,
        }
    }

    fn run(mut self) -> NodeLog {
        let mut leaders = Vec::new();
        let mut commits = Vec::new();
        for _ in 0..self.num_terms {
            self.step_term(&mut leaders, &mut commits);
        }
        NodeLog { leaders, commits }
    }

    fn step_term(&mut self, leaders: &mut Vec<LeaderEntry>, commits: &mut Vec<CommitEntry>) {
        let term = self.next_term_round().term();

        self.role = if traceforge::nondet() {
            Role::Candidate
        } else {
            Role::Follower
        };
        self.voted_for = None;
        self.current_leader = None;
        self.current_value = None;

        if self.role == Role::Candidate {
            self.voted_for = Some(self.me);
            self.broadcast(Message::RequestVote(RequestVoteMsg {
                term,
                candidate: self.me,
                sender: self.me,
            }));
        }

        let requests = self.collect_messages(self.mode, None, 0, 1);
        if let Some((stamp, msg)) = requests.first() {
            self.comm.rounds().jump(stamp.clone()).unwrap();
            match msg {
                Message::RequestVote(payload) => self.maybe_grant_vote(payload),
                Message::AppendEntries(payload) => {
                    self.role = Role::Follower;
                    self.current_leader = Some(payload.leader);
                    self.current_value = Some(payload.value);
                }
                Message::Commit(payload) => {
                    self.role = Role::Follower;
                    self.current_leader = Some(payload.leader);
                    self.current_value = Some(payload.value);
                    self.record_commit(commits, payload.term, payload.value);
                }
                Message::Vote(_) | Message::Ack(_) | Message::Init(_) => {}
            }
        }

        self.ensure_phase(Phase::Vote);
        if self.role == Role::Candidate {
            let needed = self.nodes.majority();
            let votes = self.collect_votes(0, self.nodes.len());
            let yes_votes_for_me = 1 + votes
                .iter()
                .filter(|(vote, _)| vote.granted && vote.candidate == self.me)
                .map(|(vote, _)| vote.sender)
                .collect::<HashSet<_>>()
                .len();
            if yes_votes_for_me >= needed {
                self.role = Role::Leader;
                self.current_leader = Some(self.me);
                self.record_leader(leaders, term, self.me);
            } else {
                self.role = Role::Follower;
            }
        }

        self.ensure_phase(Phase::Append);
        if self.role == Role::Leader {
            let value = Self::client_value(term, self.me);
            self.current_value = Some(value);
            self.broadcast(Message::AppendEntries(AppendEntriesMsg {
                term,
                leader: self.me,
                value,
                sender: self.me,
            }));
        }

        if self.role != Role::Leader {
            let incoming = self.collect_messages(self.mode, None, 0, 1);
            if let Some((stamp, msg)) = incoming.first() {
                self.comm.rounds().jump(stamp.clone()).unwrap();
                match msg {
                    Message::AppendEntries(payload) => {
                        self.role = Role::Follower;
                        self.current_leader = Some(payload.leader);
                        self.current_value = Some(payload.value);
                        self.ensure_phase(Phase::Ack);
                        self.comm
                            .send(
                                payload.leader,
                                Message::Ack(AckMsg {
                                    term: payload.term,
                                    leader: payload.leader,
                                    value: payload.value,
                                    sender: self.me,
                                }),
                            )
                            .unwrap();
                    }
                    Message::Commit(payload) => {
                        self.role = Role::Follower;
                        self.current_leader = Some(payload.leader);
                        self.current_value = Some(payload.value);
                        self.record_commit(commits, payload.term, payload.value);
                    }
                    Message::RequestVote(payload) => {
                        self.maybe_grant_vote(payload);
                    }
                    Message::Vote(_) | Message::Ack(_) | Message::Init(_) => {}
                }
            }
        }

        self.ensure_phase(Phase::Ack);
        if self.role == Role::Leader {
            let value = self
                .current_value
                .unwrap_or_else(|| Self::client_value(term, self.me));
            let needed_from_others = self.nodes.majority().saturating_sub(1);
            let acks = self.collect_acks(0, self.nodes.len());
            let ok = acks
                .iter()
                .filter(|(ack, _)| ack.leader == self.me && ack.value == value)
                .map(|(ack, _)| ack.sender)
                .collect::<HashSet<_>>()
                .len();

            if ok >= needed_from_others {
                self.ensure_phase(Phase::Commit);
                self.broadcast(Message::Commit(CommitMsg {
                    term,
                    leader: self.me,
                    value,
                    sender: self.me,
                }));
                self.record_commit(commits, term, value);
            }
        }

        self.ensure_phase(Phase::Commit);
        if self.role != Role::Leader {
            let commits_in = self.collect_messages(self.mode, None, 0, 1);
            if let Some((stamp, msg)) = commits_in.first() {
                self.comm.rounds().jump(stamp.clone()).unwrap();
                if let Message::Commit(payload) = msg {
                    self.current_leader = Some(payload.leader);
                    self.current_value = Some(payload.value);
                    self.record_commit(commits, payload.term, payload.value);
                }
            }
        }
    }

    fn next_term_round(&mut self) -> &RaftRound {
        if self.started {
            self.comm.rounds().advance(RaftRound::dim_term())
        } else {
            self.started = true;
            self.comm.rounds().current()
        }
    }

    fn ensure_phase(&mut self, target: Phase) -> &RaftRound {
        let current = self.comm.rounds().current().phase();
        if current == target || phase_ord(current) > phase_ord(target) {
            return self.comm.rounds().current();
        }
        while self.comm.rounds().current().phase() != target {
            self.comm.rounds().advance(RaftRound::dim_phase());
        }
        self.comm.rounds().current()
    }

    fn maybe_grant_vote(&mut self, req: &RequestVoteMsg) {
        let granted = self.voted_for.is_none() || self.voted_for == Some(req.candidate);
        if granted {
            self.voted_for = Some(req.candidate);
        }

        let term = self.ensure_phase(Phase::Vote).term();
        self.comm
            .send(
                req.candidate,
                Message::Vote(VoteMsg {
                    term,
                    candidate: req.candidate,
                    granted,
                    sender: self.me,
                }),
            )
            .unwrap();
    }

    fn record_leader(&self, leaders: &mut Vec<LeaderEntry>, term: u32, leader: ThreadId) {
        if let Some(prev) = leaders.iter().find(|entry| entry.term == term) {
            assert_eq!(
                prev.leader, leader,
                "local elected two leaders for term {}",
                term
            );
            return;
        }
        leaders.push(LeaderEntry { term, leader });
    }

    fn record_commit(&self, commits: &mut Vec<CommitEntry>, term: u32, value: u32) {
        if let Some(prev) = commits.iter().find(|entry| entry.term == term) {
            assert_eq!(
                prev.value, value,
                "local committed two values for term {}",
                term
            );
            return;
        }
        commits.push(CommitEntry { term, value });
    }

    fn client_value(term: u32, client: ThreadId) -> u32 {
        (u32::from(client) * term) % MAX_VALUE
    }

    fn broadcast(&mut self, msg: Message) {
        for node in self.nodes.iter() {
            if *node == self.me {
                continue;
            }
            self.comm.send(*node, msg.clone()).unwrap();
        }
    }

    fn collect_votes(&mut self, min: usize, max: usize) -> Vec<(VoteMsg, RaftRound)> {
        self.collect_messages(self.mode, Some(same_round_filter), min, max)
            .into_iter()
            .map(|(stamp, msg)| match msg {
                Message::Vote(vote) => (vote, stamp),
                _ => panic!("expected Vote"),
            })
            .collect()
    }

    fn collect_acks(&mut self, min: usize, max: usize) -> Vec<(AckMsg, RaftRound)> {
        self.collect_messages(self.mode, Some(same_round_filter), min, max)
            .into_iter()
            .map(|(stamp, msg)| match msg {
                Message::Ack(ack) => (ack, stamp),
                _ => panic!("expected Ack"),
            })
            .collect()
    }

    fn collect_messages(
        &mut self,
        mode: ReceiveMode,
        filter: Option<fn(&RaftRound, &RaftRound) -> bool>,
        min: usize,
        max: usize,
    ) -> Vec<(RaftRound, Message)> {
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
}

fn same_round_filter(local: &RaftRound, remote: &RaftRound) -> bool {
    local.term() == remote.term() && local.phase() == remote.phase()
}

fn start_node(num_terms: u32, mode: ReceiveMode, use_tags: bool) -> NodeLog {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, num_terms, mode, use_tags).run()
}

fn assert_leader_consistency(logs: &[NodeLog]) {
    let mut leaders: HashMap<u32, ThreadId> = HashMap::new();
    for log in logs {
        for entry in &log.leaders {
            match leaders.get(&entry.term) {
                Some(prev) => assert_eq!(*prev, entry.leader),
                None => {
                    leaders.insert(entry.term, entry.leader);
                }
            }
        }
    }
}

fn assert_commit_consistency(logs: &[NodeLog]) {
    let mut commits: HashMap<u32, u32> = HashMap::new();
    for log in logs {
        for entry in &log.commits {
            match commits.get(&entry.term) {
                Some(prev) => assert_eq!(
                    *prev, entry.value,
                    "commit disagreement at term {}: {} vs {}",
                    entry.term, prev, entry.value
                ),
                None => {
                    commits.insert(entry.term, entry.value);
                }
            }
        }
    }
}

fn run_protocol(
    num_nodes: usize,
    num_terms: u32,
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
            handles.push(thread::spawn(move || start_node(num_terms, mode, use_tags)));
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

        assert_leader_consistency(&logs);
        assert_commit_consistency(&logs);
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
    let mut num_terms = DEFAULT_NUM_TERMS;
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
            "--terms" => {
                let value = next_arg_value(&mut args, "--terms");
                num_terms = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --terms value: {value}"));
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
                    "unknown argument: {arg} (expected --nodes <n>, --terms <n>, --mode <recv|inbox>, --wo-tags, --parallel <n>, --rayon <n>)",
                );
            }
        }
    }

    (num_nodes, num_terms, mode, use_tags, parallel)
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
    let (num_nodes, num_terms, mode, use_tags, parallel) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, num_terms, mode, use_tags, parallel);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
