use std::collections::{HashMap, HashSet};

use traceforge::new_comm_close::{RoundFilter, RoundStamp, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_TERMS: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Inbox;
const DEFAULT_USE_TAGS: bool = true;
const INIT_TAG: u32 = 1;
const MAX_VALUE: u32 = 4;

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
    #[dimension("*")]
    term: u32,
    #[dimension("*")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogEntry {
    term: u32,
    value: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestVoteMsg {
    term: u32,
    candidate: ThreadId,
    last_log_index: usize,
    last_log_term: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VoteResponseMsg {
    term: u32,
    candidate: ThreadId,
    vote_granted: bool,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AppendEntriesMsg {
    term: u32,
    leader: ThreadId,
    prev_log_index: usize,
    prev_log_term: u32,
    entry: Option<LogEntry>,
    leader_commit: usize,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AppendResponseMsg {
    term: u32,
    leader: ThreadId,
    follower: ThreadId,
    success: bool,
    match_index: usize,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    RequestVote(RequestVoteMsg),
    VoteResponse(VoteResponseMsg),
    AppendEntries(AppendEntriesMsg),
    AppendResponse(AppendResponseMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeaderEntry {
    term: u32,
    leader: ThreadId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AppliedEntry {
    index: usize,
    term: u32,
    value: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeLog {
    leaders: Vec<LeaderEntry>,
    applied: Vec<AppliedEntry>,
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

    log: Vec<LogEntry>,
    commit_index: usize,
    last_applied: usize,

    next_index: HashMap<ThreadId, usize>,
    match_index: HashMap<ThreadId, usize>,
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
            log: Vec::new(),
            commit_index: 0,
            last_applied: 0,
            next_index: HashMap::new(),
            match_index: HashMap::new(),
        }
    }

    fn run(mut self) -> NodeLog {
        let mut leaders = Vec::new();
        let mut applied = Vec::new();
        for _ in 0..self.num_terms {
            self.step_term(&mut leaders, &mut applied);
        }
        self.apply_committed(&mut applied);
        NodeLog { leaders, applied }
    }

    fn step_term(&mut self, leaders: &mut Vec<LeaderEntry>, applied: &mut Vec<AppliedEntry>) {
        let term = self.next_term_round().term();

        self.role = Role::Follower;
        self.voted_for = None;
        self.current_leader = None;
        self.next_index.clear();
        self.match_index.clear();

        if traceforge::nondet() {
            self.start_election(term);
        }

        self.process_general_messages(0, 1);
        if self.current_term() != term {
            return;
        }

        self.ensure_phase(Phase::Vote);
        if self.current_term() != term {
            return;
        }
        if self.role == Role::Candidate {
            let responses = self.collect_vote_responses(self.nodes.len());
            self.handle_vote_responses(term, &responses, leaders);
        }
        if self.current_term() != term {
            return;
        }

        self.ensure_phase(Phase::Append);
        if self.current_term() != term {
            return;
        }
        if self.role == Role::Leader {
            self.leader_append_step(term);
        } else {
            self.process_general_messages(0, 1);
        }
        if self.current_term() != term {
            return;
        }

        self.ensure_phase(Phase::Ack);
        if self.current_term() != term {
            return;
        }
        if self.role == Role::Leader {
            let responses = self.collect_append_responses(self.nodes.len());
            self.handle_append_responses(term, &responses);
            if self.current_term() == term && self.role == Role::Leader {
                self.maybe_advance_commit(term);
            }
        } else {
            self.process_general_messages(0, 1);
        }
        self.apply_committed(applied);
        if self.current_term() != term {
            return;
        }

        self.ensure_phase(Phase::Commit);
        if self.current_term() != term {
            return;
        }
        if self.role == Role::Leader {
            self.broadcast_append_entries(term);
        } else {
            self.process_general_messages(0, 1);
        }
        self.apply_committed(applied);
    }

    fn next_term_round(&mut self) -> traceforge::new_comm_close::Round<RaftRound> {
        if self.started {
            self.rounds.advance(RaftRound::term())
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn ensure_phase(&mut self, target: Phase) -> traceforge::new_comm_close::Round<RaftRound> {
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

    fn current_term(&self) -> u32 {
        self.rounds.current().term()
    }

    fn last_log_index(&self) -> usize {
        self.log.len()
    }

    fn last_log_term(&self) -> u32 {
        self.log.last().map(|entry| entry.term).unwrap_or(0)
    }

    fn log_term_at(&self, index: usize) -> u32 {
        if index == 0 {
            return 0;
        }
        self.log
            .get(index.saturating_sub(1))
            .map(|entry| entry.term)
            .unwrap_or(0)
    }

    fn start_election(&mut self, term: u32) {
        self.role = Role::Candidate;
        self.voted_for = Some(self.me);
        self.current_leader = None;
        self.broadcast(Message::RequestVote(RequestVoteMsg {
            term,
            candidate: self.me,
            last_log_index: self.last_log_index(),
            last_log_term: self.last_log_term(),
            sender: self.me,
        }));
    }

    fn become_follower_at_term(&mut self, term: u32, leader_hint: Option<ThreadId>) {
        if term > self.current_term() {
            self.rounds.advance_to(RaftRound::term(), term);
        }
        self.role = Role::Follower;
        self.voted_for = None;
        self.current_leader = leader_hint;
        self.next_index.clear();
        self.match_index.clear();
    }

    fn observe_stamp_term(&mut self, stamp: &RoundStamp<RaftRound>) {
        let stamped_term = stamp.term();
        if stamped_term > self.current_term() {
            self.become_follower_at_term(stamped_term, None);
        }
    }

    fn observe_message_term(&mut self, message_term: u32, leader_hint: Option<ThreadId>) {
        if message_term > self.current_term() {
            self.become_follower_at_term(message_term, leader_hint);
        } else if message_term == self.current_term() {
            if let Some(leader) = leader_hint {
                self.current_leader = Some(leader);
            }
        }
    }

    fn process_general_messages(&mut self, min: usize, max: usize) {
        let messages = self.collect_messages(self.mode, None, min, max);
        for (msg, stamp) in messages {
            self.observe_stamp_term(&stamp);
            self.handle_non_response_message(msg);
        }
    }

    fn handle_non_response_message(&mut self, msg: Message) {
        match msg {
            Message::RequestVote(req) => self.handle_request_vote(req),
            Message::AppendEntries(req) => self.handle_append_entries(req),
            Message::VoteResponse(_) | Message::AppendResponse(_) => {}
            Message::Init(_) => panic!("unexpected Init message"),
        }
    }

    fn handle_request_vote(&mut self, req: RequestVoteMsg) {
        self.observe_message_term(req.term, None);
        let current_term = self.current_term();
        let mut granted = false;
        if req.term == current_term {
            let vote_free = self.voted_for.is_none() || self.voted_for == Some(req.candidate);
            let up_to_date = self.is_candidate_log_up_to_date(req.last_log_term, req.last_log_index);
            granted = vote_free && up_to_date;
            if granted {
                self.voted_for = Some(req.candidate);
                self.current_leader = None;
            }
        }

        self.rounds.send(
            req.candidate,
            Message::VoteResponse(VoteResponseMsg {
                term: current_term,
                candidate: req.candidate,
                vote_granted: granted,
                sender: self.me,
            }),
        );
    }

    fn is_candidate_log_up_to_date(&self, candidate_last_term: u32, candidate_last_index: usize) -> bool {
        let local_last_term = self.last_log_term();
        let local_last_index = self.last_log_index();
        if candidate_last_term != local_last_term {
            return candidate_last_term > local_last_term;
        }
        candidate_last_index >= local_last_index
    }

    fn collect_vote_responses(&mut self, max: usize) -> Vec<VoteResponseMsg> {
        let messages = self.collect_messages(self.mode, None, 0, max);
        let mut out = Vec::new();
        for (msg, stamp) in messages {
            self.observe_stamp_term(&stamp);
            match msg {
                Message::VoteResponse(resp) => out.push(resp),
                other => self.handle_non_response_message(other),
            }
        }
        out
    }

    fn handle_vote_responses(
        &mut self,
        election_term: u32,
        responses: &[VoteResponseMsg],
        leaders: &mut Vec<LeaderEntry>,
    ) {
        let mut yes_votes = HashSet::new();
        yes_votes.insert(self.me);

        for response in responses {
            self.observe_message_term(response.term, None);
            if self.current_term() != election_term || self.role != Role::Candidate {
                return;
            }
            if response.term != election_term {
                continue;
            }
            if response.candidate == self.me && response.vote_granted {
                yes_votes.insert(response.sender);
            }
        }

        if self.role == Role::Candidate
            && self.current_term() == election_term
            && yes_votes.len() >= self.nodes.majority()
        {
            self.role = Role::Leader;
            self.current_leader = Some(self.me);
            self.record_leader(leaders, election_term, self.me);
            self.init_leader_indices();
        }
    }

    fn init_leader_indices(&mut self) {
        self.next_index.clear();
        self.match_index.clear();
        let next = self.last_log_index() + 1;
        for node in self.nodes.iter() {
            if *node == self.me {
                continue;
            }
            self.next_index.insert(*node, next);
            self.match_index.insert(*node, 0);
        }
    }

    fn leader_append_step(&mut self, term: u32) {
        if traceforge::nondet() {
            let client_value = Self::client_value(term, self.me, self.last_log_index());
            self.log.push(LogEntry {
                term,
                value: client_value,
            });
        }
        self.broadcast_append_entries(term);
    }

    fn broadcast_append_entries(&self, term: u32) {
        for node in self.nodes.iter() {
            if *node == self.me {
                continue;
            }
            self.send_append_entries_to(*node, term);
        }
    }

    fn send_append_entries_to(&self, follower: ThreadId, term: u32) {
        let next = self
            .next_index
            .get(&follower)
            .copied()
            .unwrap_or(self.last_log_index() + 1)
            .max(1);
        let prev_log_index = next.saturating_sub(1);
        let prev_log_term = self.log_term_at(prev_log_index);
        let entry = if next <= self.last_log_index() {
            self.log.get(next - 1).copied()
        } else {
            None
        };

        self.rounds.send(
            follower,
            Message::AppendEntries(AppendEntriesMsg {
                term,
                leader: self.me,
                prev_log_index,
                prev_log_term,
                entry,
                leader_commit: self.commit_index,
                sender: self.me,
            }),
        );
    }

    fn handle_append_entries(&mut self, req: AppendEntriesMsg) {
        self.observe_message_term(req.term, Some(req.leader));
        let current_term = self.current_term();
        let mut success = false;
        let mut match_index = 0usize;

        if req.term == current_term {
            self.role = Role::Follower;
            self.current_leader = Some(req.leader);

            if self.matches_prev_log(req.prev_log_index, req.prev_log_term) {
                success = true;
                match_index = req.prev_log_index;
                if let Some(entry) = req.entry {
                    let entry_index = req.prev_log_index + 1;
                    self.install_entry(entry_index, entry);
                    match_index = entry_index;
                }
                if req.leader_commit > self.commit_index {
                    self.commit_index = std::cmp::min(req.leader_commit, self.last_log_index());
                }
            }
        }

        self.rounds.send(
            req.leader,
            Message::AppendResponse(AppendResponseMsg {
                term: current_term,
                leader: req.leader,
                follower: self.me,
                success,
                match_index,
                sender: self.me,
            }),
        );
    }

    fn matches_prev_log(&self, prev_index: usize, prev_term: u32) -> bool {
        if prev_index == 0 {
            return true;
        }
        if prev_index > self.last_log_index() {
            return false;
        }
        self.log_term_at(prev_index) == prev_term
    }

    fn install_entry(&mut self, index: usize, entry: LogEntry) {
        if index == 0 {
            panic!("log index must be >= 1");
        }
        if index <= self.log.len() {
            if self.log[index - 1] != entry {
                self.log.truncate(index - 1);
                self.log.push(entry);
                if self.commit_index >= index {
                    self.commit_index = index - 1;
                    if self.last_applied > self.commit_index {
                        self.last_applied = self.commit_index;
                    }
                }
            }
            return;
        }
        if index == self.log.len() + 1 {
            self.log.push(entry);
            return;
        }
        panic!(
            "invalid append index {} for log len {}",
            index,
            self.log.len()
        );
    }

    fn collect_append_responses(&mut self, max: usize) -> Vec<AppendResponseMsg> {
        let messages = self.collect_messages(self.mode, None, 0, max);
        let mut out = Vec::new();
        for (msg, stamp) in messages {
            self.observe_stamp_term(&stamp);
            match msg {
                Message::AppendResponse(resp) => out.push(resp),
                other => self.handle_non_response_message(other),
            }
        }
        out
    }

    fn handle_append_responses(&mut self, leader_term: u32, responses: &[AppendResponseMsg]) {
        for response in responses {
            self.observe_message_term(response.term, None);
            if self.current_term() != leader_term || self.role != Role::Leader {
                return;
            }
            if response.term != leader_term || response.leader != self.me {
                continue;
            }

            if response.success {
                let current_match = self.match_index.get(&response.follower).copied().unwrap_or(0);
                let next_match = current_match.max(response.match_index);
                self.match_index.insert(response.follower, next_match);
                self.next_index.insert(response.follower, next_match + 1);
            } else {
                let current_next = self
                    .next_index
                    .get(&response.follower)
                    .copied()
                    .unwrap_or(self.last_log_index() + 1);
                let backtracked = current_next.saturating_sub(1).max(1);
                self.next_index.insert(response.follower, backtracked);
                self.send_append_entries_to(response.follower, leader_term);
            }
        }
    }

    fn maybe_advance_commit(&mut self, current_term: u32) {
        if self.last_log_index() <= self.commit_index {
            return;
        }

        for index in ((self.commit_index + 1)..=self.last_log_index()).rev() {
            if self.log_term_at(index) != current_term {
                continue;
            }
            let mut replicated = 1;
            for node in self.nodes.iter() {
                if *node == self.me {
                    continue;
                }
                let matched = self.match_index.get(node).copied().unwrap_or(0);
                if matched >= index {
                    replicated += 1;
                }
            }
            if replicated >= self.nodes.majority() {
                self.commit_index = index;
                return;
            }
        }
    }

    fn apply_committed(&mut self, applied: &mut Vec<AppliedEntry>) {
        while self.last_applied < self.commit_index {
            self.last_applied += 1;
            let entry = self.log[self.last_applied - 1];
            applied.push(AppliedEntry {
                index: self.last_applied,
                term: entry.term,
                value: entry.value,
            });
        }
    }

    fn record_leader(&self, leaders: &mut Vec<LeaderEntry>, term: u32, leader: ThreadId) {
        if let Some(previous) = leaders.iter().find(|entry| entry.term == term) {
            assert_eq!(
                previous.leader, leader,
                "local elected two leaders for term {}",
                term
            );
            return;
        }
        leaders.push(LeaderEntry { term, leader });
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            if *node == self.me {
                continue;
            }
            self.rounds.send(*node, msg.clone());
        }
    }

    fn client_value(term: u32, client: ThreadId, seq: usize) -> u32 {
        (u32::from(client) + term + (seq as u32)) % MAX_VALUE
    }

    fn collect_messages(
        &self,
        mode: ReceiveMode,
        filter: Option<&RoundFilter<RaftRound>>,
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
                msgs.into_iter()
                    .flatten()
                    .map(|msg| (msg.payload().clone(), msg.stamp().clone()))
                    .collect()
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
    let mut leaders: HashMap<u32, ThreadId> = HashMap::new();
    for log in logs {
        for entry in &log.leaders {
            match leaders.get(&entry.term) {
                Some(previous) => assert_eq!(*previous, entry.leader),
                None => {
                    leaders.insert(entry.term, entry.leader);
                }
            }
        }
    }
}

fn assert_committed_prefix_consistency(logs: &[NodeLog]) {
    let mut by_index: HashMap<usize, (u32, u32)> = HashMap::new();
    for log in logs {
        for (offset, entry) in log.applied.iter().enumerate() {
            assert_eq!(entry.index, offset + 1);
            match by_index.get(&entry.index) {
                Some((term, value)) => {
                    assert_eq!(*term, entry.term);
                    assert_eq!(*value, entry.value);
                }
                None => {
                    by_index.insert(entry.index, (entry.term, entry.value));
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
        assert_committed_prefix_consistency(&logs);
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

fn main() {
    let (num_nodes, num_terms, mode, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, num_terms, mode, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
