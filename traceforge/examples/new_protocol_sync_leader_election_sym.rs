use std::collections::BTreeMap;

use traceforge::new_comm_close::{MatchKind, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum Phase {
    NewBallot,
    AckBallot,
}

#[derive(Clone, traceforge::Round)]
struct LeaderRound {
    #[dimension("=")]
    ballot: u32,
    #[dimension("=")]
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
struct SymmetryPlan {
    proposers_by_ballot: Vec<Vec<bool>>,
}

impl SymmetryPlan {
    fn is_proposer(&self, ballot_index: usize, node_index: usize) -> bool {
        self.proposers_by_ballot[ballot_index][node_index]
    }

    fn expected_ballots(&self) -> usize {
        self.proposers_by_ballot.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InitMsg {
    participants: Participants,
    proposer_plan: SymmetryPlan,
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
    Init(InitMsg),
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
    me_index: usize,
    rounds: Rounds<LeaderRound>,
    leader: ThreadId,
    started: bool,
    num_ballots: u32,
    mode: ReceiveMode,
    proposer_plan: SymmetryPlan,
}

impl Node {
    fn new(
        nodes: Participants,
        num_ballots: u32,
        mode: ReceiveMode,
        use_tags: bool,
        proposer_plan: SymmetryPlan,
    ) -> Self {
        let me = thread::current().id();
        let me_index = nodes
            .iter()
            .position(|id| *id == me)
            .unwrap_or_else(|| panic!("node not found in participants"));
        let expected_ballots =
            usize::try_from(num_ballots).unwrap_or_else(|_| panic!("num_ballots does not fit usize"));
        assert_eq!(
            proposer_plan.expected_ballots(),
            expected_ballots,
            "proposer plan ballots mismatch",
        );
        let rounds = if use_tags {
            Rounds::<LeaderRound>::new()
        } else {
            Rounds::<LeaderRound>::new_wo_tags(false)
        };
        Self {
            nodes,
            me,
            me_index,
            rounds,
            leader: me,
            started: false,
            num_ballots,
            mode,
            proposer_plan,
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
        let ballot_round = self.next_round();
        if self.coord(&ballot_round) {
            self.phase_new_ballot_as_leader();
            self.phase_ack_ballot(log);
        } else if self.phase_new_ballot_as_follower() {
            self.phase_ack_ballot(log);
        }
    }

    fn phase_new_ballot_as_leader(&mut self) {
        self.broadcast(Message::NewBallot(NewBallotMsg {
            leader: self.me,
            sender: self.me,
        }));
        self.leader = self.me;
        self.rounds.advance(LeaderRound::phase());
    }

    fn phase_new_ballot_as_follower(&mut self) -> bool {
        let new_ballot_msgs = self.collect_new_ballot();
        if new_ballot_msgs.len() != 1 {
            return false;
        }

        match &new_ballot_msgs[0] {
            Message::NewBallot(payload) => self.leader = payload.leader,
            _ => panic!("expected NewBallot"),
        }

        self.enter_ack_ballot_phase_if_needed();
        true
    }

    fn enter_ack_ballot_phase_if_needed(&mut self) {
        if self.rounds.current().phase() == Phase::NewBallot {
            self.rounds.advance(LeaderRound::phase());
        }
    }

    fn phase_ack_ballot(&mut self, log: &mut Vec<LogEntry>) {
        let ballot = self.rounds.current().ballot();
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

    fn next_round(&mut self) -> traceforge::new_comm_close::Round<LeaderRound> {
        if self.started {
            self.rounds.advance(LeaderRound::ballot())
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn coord(&self, round: &traceforge::new_comm_close::Round<LeaderRound>) -> bool {
        let ballot_index = round.ballot() as usize;
        if ballot_index >= self.proposer_plan.expected_ballots() {
            return false;
        }
        self.proposer_plan.is_proposer(ballot_index, self.me_index)
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            self.rounds.send(*node, msg.clone());
        }
    }

    fn collect_new_ballot(&self) -> Vec<Message> {
        let filter = self
            .rounds
            .filter()
            .ballot(MatchKind::Eq)
            .phase(MatchKind::Eq);
        self.collect_messages(self.mode, Some(&filter), 0, 1)
    }

    fn collect_ack_ballot(&self, max: usize) -> Vec<AckBallotMsg> {
        let filter = self
            .rounds
            .filter()
            .ballot(MatchKind::Eq)
            .phase(MatchKind::Eq);
        self.collect_messages(self.mode, Some(&filter), 0, max)
            .into_iter()
            .map(expect_ack_ballot)
            .collect()
    }

    fn collect_messages(
        &self,
        mode: ReceiveMode,
        filter: Option<&traceforge::new_comm_close::RoundFilter<LeaderRound>>,
        min: usize,
        max: usize,
    ) -> Vec<Message> {
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
                    out.push(msg.payload().clone());
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
                    out.push(msg.payload().clone());
                }
                out
            }
        }
    }

    fn all_same_leader(messages: &[AckBallotMsg], leader: ThreadId) -> bool {
        messages.iter().all(|msg| msg.leader == leader)
    }
}

fn expect_ack_ballot(msg: Message) -> AckBallotMsg {
    match msg {
        Message::AckBallot(payload) => payload,
        _ => panic!("expected AckBallotMsg"),
    }
}

fn start_node(num_ballots: u32, mode: ReceiveMode, use_tags: bool) -> Vec<LogEntry> {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let init = match init {
        Message::Init(init) => init,
        _ => panic!("expected init message"),
    };
    Node::new(
        init.participants,
        num_ballots,
        mode,
        use_tags,
        init.proposer_plan,
    )
    .run()
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

fn partition_by_signature(signatures: &[Vec<bool>]) -> Vec<Vec<usize>> {
    let mut by_signature: BTreeMap<Vec<bool>, Vec<usize>> = BTreeMap::new();
    for (node_index, signature) in signatures.iter().enumerate() {
        by_signature
            .entry(signature.clone())
            .or_default()
            .push(node_index);
    }
    by_signature.into_values().collect()
}

fn build_symmetry_reduced_plan(num_nodes: usize, num_ballots: u32) -> SymmetryPlan {
    let mut proposer_signatures = vec![Vec::<bool>::new(); num_nodes];
    let mut proposers_by_ballot = Vec::new();

    for _ballot in 0..num_ballots {
        let symmetry_classes = partition_by_signature(&proposer_signatures);
        let mut ballot_proposers = vec![false; num_nodes];

        for class in symmetry_classes {
            let count = (0..=class.len()).nondet();
            for node_index in class.into_iter().take(count) {
                ballot_proposers[node_index] = true;
            }
        }

        for node_index in 0..num_nodes {
            proposer_signatures[node_index].push(ballot_proposers[node_index]);
        }
        proposers_by_ballot.push(ballot_proposers);
    }

    SymmetryPlan {
        proposers_by_ballot,
    }
}

fn run_protocol(
    num_nodes: usize,
    num_ballots: u32,
    mode: ReceiveMode,
    use_tags: bool,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let proposer_plan = build_symmetry_reduced_plan(num_nodes, num_ballots);
        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            handles.push(thread::spawn(move || start_node(num_ballots, mode, use_tags)));
        }
        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            if use_tags {
                traceforge::send_msg(
                    handle.thread().id(),
                    Message::Init(InitMsg {
                        participants: nodes.clone(),
                        proposer_plan: proposer_plan.clone(),
                    }),
                );
            } else {
                traceforge::send_tagged_msg(
                    handle.thread().id(),
                    INIT_TAG,
                    Message::Init(InitMsg {
                        participants: nodes.clone(),
                        proposer_plan: proposer_plan.clone(),
                    }),
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

fn parse_args() -> (usize, u32, ReceiveMode, bool) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_ballots = DEFAULT_NUM_BALLOTS;
    let mut mode = DEFAULT_MODE;
    let mut use_tags = DEFAULT_USE_TAGS;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nodes" => {
                let value = next_arg_value(&mut args, "--nodes");
                num_nodes = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --nodes value: {}", value));
            }
            "--ballots" => {
                let value = next_arg_value(&mut args, "--ballots");
                num_ballots = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --ballots value: {}", value));
            }
            "--mode" => {
                let value = next_arg_value(&mut args, "--mode");
                mode = parse_mode(&value);
            }
            "--wo-tags" => {
                use_tags = false;
            }
            _ => {
                panic!(
                    "unknown argument: {} (expected --nodes <n>, --ballots <n>, --mode <recv|inbox>, --wo-tags)",
                    arg
                );
            }
        }
    }

    (num_nodes, num_ballots, mode, use_tags)
}

fn next_arg_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{} requires a value", flag))
}

fn parse_mode(value: &str) -> ReceiveMode {
    match value {
        "recv" => ReceiveMode::Recv,
        "inbox" => ReceiveMode::Inbox,
        _ => panic!("invalid --mode value: {} (expected recv or inbox)", value),
    }
}

fn main() {
    let (num_nodes, num_ballots, mode, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, num_ballots, mode, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
