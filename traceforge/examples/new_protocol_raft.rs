use traceforge::comm_close::{MatchKind, RoundStamp, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum Phase {
    RequestVote,
    Vote,
    Append,
    Ack,
    Commit,
}

#[derive(Clone, traceforge::Round)]
struct RaftRound {
    term: u32,
    phase: Phase,
}

fn phase_ord(p: Phase) -> u32 {
    match p {
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
    rounds: Rounds<RaftRound>,
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
        let me = thread::current().id();
        let rounds = if use_tags {
            Rounds::<RaftRound>::new()
        } else {
            Rounds::<RaftRound>::new_wo_tags(false)
        };
        Self {
            nodes,
            me,
            rounds,
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
        let rv_round = self.next_term_round();
        let term = rv_round.term();

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
        if let Some((msg, stamp)) = requests.first() {
            self.rounds.jump(stamp);
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

        let _vote_round = self.ensure_phase(Phase::Vote);
        if self.role == Role::Candidate {
            let needed = self.nodes.majority();
            let votes = self.collect_votes(0, self.nodes.len());
            let yes_votes_for_me = 1
                + votes
                    .iter()
                    .filter(|(vote, _)| vote.granted && vote.candidate == self.me)
                    .map(|(vote, _)| vote.sender)
                    .collect::<std::collections::HashSet<_>>()
                    .len();
            if yes_votes_for_me >= needed {
                self.role = Role::Leader;
                self.current_leader = Some(self.me);
                self.record_leader(leaders, term, self.me);
            } else {
                self.role = Role::Follower;
            }
        }

        let _append_round = self.ensure_phase(Phase::Append);
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
            if let Some((msg, stamp)) = incoming.first() {
                self.rounds.jump(stamp);
                match msg {
                    Message::AppendEntries(payload) => {
                        self.role = Role::Follower;
                        self.current_leader = Some(payload.leader);
                        self.current_value = Some(payload.value);
                        let _ack_round = self.ensure_phase(Phase::Ack);
                        self.rounds.send(
                            payload.leader,
                            Message::Ack(AckMsg {
                                term: payload.term,
                                leader: payload.leader,
                                value: payload.value,
                                sender: self.me,
                            }),
                        );
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

        let _ack_round = self.ensure_phase(Phase::Ack);
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
                .collect::<std::collections::HashSet<_>>()
                .len();

            if ok >= needed_from_others {
                let _commit_round = self.ensure_phase(Phase::Commit);
                self.broadcast(Message::Commit(CommitMsg {
                    term,
                    leader: self.me,
                    value,
                    sender: self.me,
                }));
                self.record_commit(commits, term, value);
            }
        }

        let _commit_round = self.ensure_phase(Phase::Commit);
        if self.role != Role::Leader {
            let commits_in = self.collect_messages(self.mode, None, 0, 1);
            if let Some((msg, stamp)) = commits_in.first() {
                self.rounds.jump(stamp);
                if let Message::Commit(payload) = msg {
                    self.current_leader = Some(payload.leader);
                    self.current_value = Some(payload.value);
                    self.record_commit(commits, payload.term, payload.value);
                }
            }
        }
    }

    fn next_term_round(&mut self) -> traceforge::comm_close::Round<RaftRound> {
        if self.started {
            self.rounds.advance(RaftRound::term())
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn ensure_phase(&mut self, target: Phase) -> traceforge::comm_close::Round<RaftRound> {
        let mut round = self.rounds.current();
        let current = round.phase();
        if current == target {
            return round;
        }
        if phase_ord(current) > phase_ord(target) {
            return round;
        }
        while round.phase() != target {
            round = self.rounds.advance(RaftRound::phase());
        }
        round
    }

    fn maybe_grant_vote(&mut self, req: &RequestVoteMsg) {
        let granted = self.voted_for.is_none() || self.voted_for == Some(req.candidate);
        if granted {
            self.voted_for = Some(req.candidate);
        }

        let vote_round = self.ensure_phase(Phase::Vote);
        self.rounds.send(
            req.candidate,
            Message::Vote(VoteMsg {
                term: vote_round.term(),
                candidate: req.candidate,
                granted,
                sender: self.me,
            }),
        );
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

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            if *node == self.me {
                continue;
            }
            self.rounds.send(*node, msg.clone());
        }
    }

    fn collect_votes(&self, min: usize, max: usize) -> Vec<(VoteMsg, RoundStamp<RaftRound>)> {
        let filter = self
            .rounds
            .filter()
            .term(MatchKind::Eq)
            .phase(MatchKind::Eq);
        self.collect_messages(self.mode, Some(&filter), min, max)
            .into_iter()
            .map(|(msg, stamp)| match msg {
                Message::Vote(vote) => (vote, stamp),
                _ => panic!("expected Vote"),
            })
            .collect()
    }

    fn collect_acks(&self, min: usize, max: usize) -> Vec<(AckMsg, RoundStamp<RaftRound>)> {
        let filter = self
            .rounds
            .filter()
            .term(MatchKind::Eq)
            .phase(MatchKind::Eq);
        self.collect_messages(self.mode, Some(&filter), min, max)
            .into_iter()
            .map(|(msg, stamp)| match msg {
                Message::Ack(ack) => (ack, stamp),
                _ => panic!("expected Ack"),
            })
            .collect()
    }

    fn collect_messages(
        &self,
        mode: ReceiveMode,
        filter: Option<&traceforge::comm_close::RoundFilter<RaftRound>>,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp<RaftRound>)> {
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
                        Some(filter) => self.rounds.recv_block_with::<Message>(filter),
                        None => self.rounds.recv_block::<Message>(),
                    };
                    out.push((msg.payload().clone(), msg.stamp().clone()));
                }
                out
            }
            ReceiveMode::Inbox => {
                let msgs = match filter {
                    Some(filter) => self
                        .rounds
                        .inbox_with_bounds_with::<Message>(filter, min, Some(max)),
                    None => self.rounds.inbox_with_bounds::<Message>(min, Some(max)),
                };
                let mut out = Vec::new();
                for msg in msgs.into_iter().flatten() {
                    out.push((msg.payload().clone(), msg.stamp().clone()));
                }
                out
            }
        }
    }
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
    use std::collections::HashMap;
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
    use std::collections::HashMap;
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
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            handles.push(thread::spawn(move || start_node(num_terms, mode, use_tags)));
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

        assert_leader_consistency(&logs);
        assert_commit_consistency(&logs);
    })
}

fn parse_args() -> (usize, u32, ReceiveMode, bool) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_terms = DEFAULT_NUM_TERMS;
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
            "--terms" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| panic!("--terms requires a value"));
                num_terms = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --terms value: {}", value));
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
                    "unknown argument: {} (expected --nodes <n>, --terms <n>, --mode <recv|inbox>, --wo-tags)",
                    arg
                );
            }
        }
    }

    (num_nodes, num_terms, mode, use_tags)
}

fn parse_mode(value: &str) -> ReceiveMode {
    match value {
        "recv" => ReceiveMode::Recv,
        "inbox" => ReceiveMode::Inbox,
        _ => panic!("invalid --mode value: {} (expected recv or inbox)", value),
    }
}

fn main() {
    let (num_nodes, num_terms, mode, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, num_terms, mode, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
