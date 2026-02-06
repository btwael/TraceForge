use traceforge::comm_close::{self, DefaultMatch, RoundScheme, RoundStamp, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_TERMS: u32 = 1;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundKey)]
enum Key {
    Term,
    Phase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum Phase {
    RequestVote,
    Vote,
    Append,
    Ack,
    Commit,
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
    rounds: Rounds,
    started: bool,
    num_terms: u32,
    mode: ReceiveMode,

    // term-local state
    role: Role,
    voted_for: Option<ThreadId>,
    current_leader: Option<ThreadId>,
    current_value: Option<u32>,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme, num_terms: u32, mode: ReceiveMode) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
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
        let term = rv_round.get_u32(Key::Term);

        // Reset term-local state.
        self.role = if traceforge::nondet() {
            Role::Candidate
        } else {
            Role::Follower
        };
        self.voted_for = None;
        self.current_leader = None;
        self.current_value = None;

        // Phase 1: RequestVote
        if self.role == Role::Candidate {
            self.voted_for = Some(self.me); // self-vote
            let msg = RequestVoteMsg {
                term,
                candidate: self.me,
                sender: self.me,
            };
            self.broadcast(Message::RequestVote(msg));
        }

        // A follower can vote for at most one candidate. We model "timeout" by receiving 0 messages.
        // We also allow catch-up: a node may receive future-term messages and jump.
        let requests = self.collect_messages(self.mode, &rv_round, 0, 1);
        if let Some((msg, stamp)) = requests.first() {
            self.rounds.jump(stamp);
            match msg {
                Message::RequestVote(payload) => {
                    self.maybe_grant_vote(payload);
                }
                Message::AppendEntries(payload) => {
                    // Received a leader heartbeat / append in (possibly) a higher term.
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
                Message::Vote(_) | Message::Ack(_) | Message::Init(_) => {
                    // Ignore unexpected types here.
                }
            }
        }

        // Phase 2: Vote
        let vote_round = self.ensure_phase(Phase::Vote);

        if self.role == Role::Candidate {
            let needed = self.nodes.majority();
            let votes = self.collect_votes(&vote_round, 0, self.nodes.len());
            let yes_votes_for_me = 1
                + votes
                    .iter()
                    .filter(|(v, _)| v.granted && v.candidate == self.me)
                    .map(|(v, _)| v.sender)
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

        // Phase 3: AppendEntries
        let append_round = self.ensure_phase(Phase::Append);

        if self.role == Role::Leader {
            let value = Self::client_value(term, self.me);
            self.current_value = Some(value);
            let msg = AppendEntriesMsg {
                term,
                leader: self.me,
                value,
                sender: self.me,
            };
            self.broadcast(Message::AppendEntries(msg));
        }

        // Followers try to receive an AppendEntries (or future term messages) and then send Ack in Ack phase.
        if self.role != Role::Leader {
            let incoming = self.collect_messages(self.mode, &append_round, 0, 1);
            if let Some((msg, stamp)) = incoming.first() {
                self.rounds.jump(stamp);
                match msg {
                    Message::AppendEntries(payload) => {
                        self.role = Role::Follower;
                        self.current_leader = Some(payload.leader);
                        self.current_value = Some(payload.value);

                        let _ack_round = self.ensure_phase(Phase::Ack);
                        let ack = AckMsg {
                            term: payload.term,
                            leader: payload.leader,
                            value: payload.value,
                            sender: self.me,
                        };
                        comm_close::send(payload.leader, Message::Ack(ack));
                    }
                    Message::Commit(payload) => {
                        self.role = Role::Follower;
                        self.current_leader = Some(payload.leader);
                        self.current_value = Some(payload.value);
                        self.record_commit(commits, payload.term, payload.value);
                    }
                    Message::RequestVote(payload) => {
                        // Future term election
                        self.maybe_grant_vote(payload);
                    }
                    _ => {
                        // Ignore.
                    }
                }
            }
        }

        // Phase 4: Ack (leader collects)
        let ack_round = self.ensure_phase(Phase::Ack);

        if self.role == Role::Leader {
            let value = self
                .current_value
                .unwrap_or_else(|| Self::client_value(term, self.me));
            let needed_from_others = self.nodes.majority().saturating_sub(1);
            let acks = self.collect_acks(&ack_round, 0, self.nodes.len());
            let ok = acks
                .iter()
                .filter(|(a, _)| a.leader == self.me && a.value == value)
                .map(|(a, _)| a.sender)
                .collect::<std::collections::HashSet<_>>()
                .len();

            if ok >= needed_from_others {
                // Phase 5: Commit
                let _commit_round = self.ensure_phase(Phase::Commit);
                let commit = CommitMsg {
                    term,
                    leader: self.me,
                    value,
                    sender: self.me,
                };
                self.broadcast(Message::Commit(commit));
                self.record_commit(commits, term, value);
            }
        }

        // Followers learn commit (if they didn't already).
        let commit_round = self.ensure_phase(Phase::Commit);
        if self.role != Role::Leader {
            let commits_in = self.collect_messages(self.mode, &commit_round, 0, 1);
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

    fn next_term_round(&mut self) -> comm_close::Round {
        if self.started {
            self.rounds.advance(Key::Term)
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn ensure_phase(&mut self, target: Phase) -> comm_close::Round {
        let mut r = self.rounds.current();
        let current = r.get_enum::<_, Phase>(Key::Phase);
        if current == target {
            return r;
        }
        if phase_ord(current) > phase_ord(target) {
            // Can't go backwards.
            return r;
        }
        while r.get_enum::<_, Phase>(Key::Phase) != target {
            r = self.rounds.advance(Key::Phase);
        }
        r
    }

    fn maybe_grant_vote(&mut self, req: &RequestVoteMsg) {
        // If we've already voted in this term, we don't grant another.
        // (We don't model log freshness here; simplified.)
        let granted = self.voted_for.is_none() || self.voted_for == Some(req.candidate);
        if granted {
            self.voted_for = Some(req.candidate);
        }

        let vote_round = self.ensure_phase(Phase::Vote);
        let term = vote_round.get_u32(Key::Term);
        let msg = VoteMsg {
            term,
            candidate: req.candidate,
            granted,
            sender: self.me,
        };
        comm_close::send(req.candidate, Message::Vote(msg));
    }

    fn record_leader(&self, leaders: &mut Vec<LeaderEntry>, term: u32, leader: ThreadId) {
        if let Some(prev) = leaders.iter().find(|e| e.term == term) {
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
        if let Some(prev) = commits.iter().find(|e| e.term == term) {
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
            comm_close::send(*node, msg.clone());
        }
    }

    fn collect_votes(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(VoteMsg, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Term, DefaultMatch::Eq)
            .level_cmp(Key::Phase, DefaultMatch::Eq);
        self.collect_messages_with_filter(self.mode, round, &filter, min, max)
            .into_iter()
            .map(|(msg, stamp)| match msg {
                Message::Vote(v) => (v, stamp),
                _ => panic!("expected Vote"),
            })
            .collect()
    }

    fn collect_acks(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(AckMsg, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Term, DefaultMatch::Eq)
            .level_cmp(Key::Phase, DefaultMatch::Eq);
        self.collect_messages_with_filter(self.mode, round, &filter, min, max)
            .into_iter()
            .map(|(msg, stamp)| match msg {
                Message::Ack(a) => (a, stamp),
                _ => panic!("expected Ack"),
            })
            .collect()
    }

    fn collect_messages(
        &self,
        mode: ReceiveMode,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        assert!(max >= min, "requires max >= min");
        match mode {
            ReceiveMode::Recv => {
                let mut out = Vec::new();
                let count = if max == min { min } else { (min..=max).nondet() };
                for _ in 0..count {
                    let msg = comm_close::recv_block::<Message>(round);
                    out.push((msg.payload(round).clone(), msg.round_stamp()));
                }
                out
            }
            ReceiveMode::Inbox => {
                let filter = round.filter();
                let msgs = comm_close::inbox_with_bounds_filter(&filter, min, Some(max));
                let mut out = Vec::new();
                for msg in msgs.into_iter().flatten() {
                    let payload = msg
                        .payload(round)
                        .as_any_ref()
                        .downcast_ref::<Message>()
                        .cloned()
                        .expect("expected Message payload");
                    out.push((payload, msg.round_stamp()));
                }
                out
            }
        }
    }

    fn collect_messages_with_filter(
        &self,
        mode: ReceiveMode,
        round: &comm_close::Round,
        filter: &comm_close::RoundFilter,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        assert!(max >= min, "requires max >= min");
        match mode {
            ReceiveMode::Recv => {
                let mut out = Vec::new();
                let count = if max == min { min } else { (min..=max).nondet() };
                for _ in 0..count {
                    let msg = comm_close::recv_block_with_filter::<Message>(filter);
                    out.push((msg.payload(round).clone(), msg.round_stamp()));
                }
                out
            }
            ReceiveMode::Inbox => {
                let msgs = comm_close::inbox_with_bounds_filter(filter, min, Some(max));
                let mut out = Vec::new();
                for msg in msgs.into_iter().flatten() {
                    let payload = msg
                        .payload(round)
                        .as_any_ref()
                        .downcast_ref::<Message>()
                        .cloned()
                        .expect("expected Message payload");
                    out.push((payload, msg.round_stamp()));
                }
                out
            }
        }
    }
}

fn start_node(scheme: RoundScheme, num_terms: u32, mode: ReceiveMode) -> NodeLog {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, scheme, num_terms, mode).run()
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

fn run_protocol(num_nodes: usize, num_terms: u32, mode: ReceiveMode) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let scheme = RoundScheme::builder()
            .from_u32(Key::Term)
            .from_enum::<Phase>(Key::Phase)
            .build();

        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            let scheme = scheme.clone();
            let num_terms = num_terms;
            let mode = mode;
            handles.push(thread::spawn(move || start_node(scheme, num_terms, mode)));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(nodes.clone()));
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }

        assert_leader_consistency(&logs);
        assert_commit_consistency(&logs);
    })
}

fn run_protocol_with_recv(num_nodes: usize, num_terms: u32) -> traceforge::Stats {
    run_protocol(num_nodes, num_terms, ReceiveMode::Recv)
}

fn run_protocol_with_inbox(num_nodes: usize, num_terms: u32) -> traceforge::Stats {
    run_protocol(num_nodes, num_terms, ReceiveMode::Inbox)
}

fn parse_args() -> (usize, u32, ReceiveMode) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_terms = DEFAULT_NUM_TERMS;
    let mut mode: Option<ReceiveMode> = None;
    let mut args = std::env::args().skip(1).peekable();

    while let Some(arg) = args.next() {
        if arg == "recv" {
            mode = Some(ReceiveMode::Recv);
        } else if arg == "inbox" {
            mode = Some(ReceiveMode::Inbox);
        } else if arg == "--nodes" {
            let value = args.next().unwrap_or_else(|| panic!("--nodes requires a value"));
            num_nodes = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid --nodes value: {}", value));
        } else if arg == "--terms" {
            let value = args.next().unwrap_or_else(|| panic!("--terms requires a value"));
            num_terms = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid --terms value: {}", value));
        } else {
            panic!("unknown argument: {}", arg);
        }
    }

    let mode = mode.unwrap_or_else(|| panic!("Must specify recv or inbox!"));
    (num_nodes, num_terms, mode)
}

fn main() {
    let (num_nodes, num_terms, mode) = parse_args();
    let stats = match mode {
        ReceiveMode::Recv => run_protocol_with_recv(num_nodes, num_terms),
        ReceiveMode::Inbox => run_protocol_with_inbox(num_nodes, num_terms),
    };
    println!("Stats = {}, {}", stats.execs, stats.block);
}
