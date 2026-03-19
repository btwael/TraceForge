use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{mpsc, Arc, Mutex};

use traceforge::comm_close::{MatchKind, RoundStamp, Rounds};
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
            self.run_leader_round(log);
        } else {
            self.run_follower_round(log);
        }
    }

    fn run_leader_round(&mut self, log: &mut Vec<LogEntry>) {
        // phase 1: NewBallot
        self.phase_new_ballot_as_leader();

        // phase 2: AckBallot
        self.phase_ack_ballot(log);
    }

    fn phase_new_ballot_as_leader(&mut self) {
        self.broadcast(Message::NewBallot(NewBallotMsg {
            leader: self.me,
            sender: self.me,
        }));
        self.leader = self.me;
        self.rounds.advance(LeaderRound::phase());
    }

    fn run_follower_round(&mut self, log: &mut Vec<LogEntry>) {
        // phase 1: NewBallot
        let new_ballot_msgs = self.collect_new_ballot();
        if new_ballot_msgs.len() == 1 {
            let (msg, stamp) = &new_ballot_msgs[0];
            self.rounds.jump(stamp);
            match msg {
                Message::NewBallot(payload) => self.leader = payload.leader,
                Message::AckBallot(payload) => self.leader = payload.leader,
                _ => panic!("expected NewBallot or AckBallot"),
            }

            // phase 2: AckBallot
            self.enter_ack_ballot_phase_if_needed();
            self.phase_ack_ballot(log);
        }
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

    fn next_round(&mut self) -> traceforge::comm_close::Round<LeaderRound> {
        if self.started {
            self.rounds.advance(LeaderRound::ballot())
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn coord(&self, round: &traceforge::comm_close::Round<LeaderRound>) -> bool {
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

    fn collect_new_ballot(&self) -> Vec<(Message, RoundStamp<LeaderRound>)> {
        self.collect_messages(self.mode, None, 0, 1)
    }

    fn collect_ack_ballot(&self, max: usize) -> Vec<(AckBallotMsg, RoundStamp<LeaderRound>)> {
        let filter = self
            .rounds
            .filter()
            .ballot(MatchKind::Eq)
            .phase(MatchKind::Eq);
        self.collect_messages(self.mode, Some(&filter), 0, max)
            .into_iter()
            .map(|(msg, stamp)| match msg {
                Message::AckBallot(payload) => (payload, stamp),
                _ => panic!("expected AckBallotMsg"),
            })
            .collect()
    }

    fn collect_messages(
        &self,
        mode: ReceiveMode,
        filter: Option<&traceforge::comm_close::RoundFilter<LeaderRound>>,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp<LeaderRound>)> {
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
                    out.push((msg.payload().clone(), (*msg.stamp()).clone()));
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
                    out.push((msg.payload().clone(), (*msg.stamp()).clone()));
                }
                out
            }
        }
    }

    fn all_same_leader(messages: &[(AckBallotMsg, RoundStamp<LeaderRound>)], leader: ThreadId) -> bool {
        messages.iter().all(|(msg, _)| msg.leader == leader)
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

fn build_ballot_options(num_nodes: usize, classes: &[Vec<usize>]) -> Vec<Vec<bool>> {
    fn rec(
        class_idx: usize,
        classes: &[Vec<usize>],
        ballot_proposers: &mut [bool],
        out: &mut Vec<Vec<bool>>,
    ) {
        if class_idx == classes.len() {
            out.push(ballot_proposers.to_vec());
            return;
        }
        let class = &classes[class_idx];
        for node in class {
            ballot_proposers[*node] = false;
        }
        for count in 0..=class.len() {
            if count > 0 {
                ballot_proposers[class[count - 1]] = true;
            }
            rec(class_idx + 1, classes, ballot_proposers, out);
        }
    }

    let mut out = Vec::new();
    let mut ballot_proposers = vec![false; num_nodes];
    rec(0, classes, &mut ballot_proposers, &mut out);
    out
}

fn enumerate_symmetry_reduced_plans<F>(
    num_nodes: usize,
    num_ballots: u32,
    on_plan: &mut F,
) -> usize
where
    F: FnMut(SymmetryPlan) -> bool,
{
    fn rec<F>(
        ballot_idx: usize,
        num_nodes: usize,
        num_ballots: usize,
        proposer_signatures: &mut [Vec<bool>],
        proposers_by_ballot: &mut Vec<Vec<bool>>,
        produced: &mut usize,
        on_plan: &mut F,
    ) -> bool
    where
        F: FnMut(SymmetryPlan) -> bool,
    {
        if ballot_idx == num_ballots {
            *produced += 1;
            let plan = SymmetryPlan {
                proposers_by_ballot: proposers_by_ballot.clone(),
            };
            return on_plan(plan);
        }

        let classes = partition_by_signature(proposer_signatures);
        for ballot_proposers in build_ballot_options(num_nodes, &classes) {
            for node_index in 0..num_nodes {
                proposer_signatures[node_index].push(ballot_proposers[node_index]);
            }
            proposers_by_ballot.push(ballot_proposers);

            if !rec(
                ballot_idx + 1,
                num_nodes,
                num_ballots,
                proposer_signatures,
                proposers_by_ballot,
                produced,
                on_plan,
            ) {
                proposers_by_ballot.pop();
                for node_index in 0..num_nodes {
                    proposer_signatures[node_index].pop();
                }
                return false;
            }

            proposers_by_ballot.pop();
            for node_index in 0..num_nodes {
                proposer_signatures[node_index].pop();
            }
        }
        true
    }

    let num_ballots = usize::try_from(num_ballots).expect("num_ballots does not fit usize");
    let mut proposer_signatures = vec![Vec::<bool>::new(); num_nodes];
    let mut proposers_by_ballot = Vec::new();
    let mut produced = 0;
    let _ = rec(
        0,
        num_nodes,
        num_ballots,
        &mut proposer_signatures,
        &mut proposers_by_ballot,
        &mut produced,
        on_plan,
    );
    produced
}

fn run_protocol_for_plan(
    num_nodes: usize,
    num_ballots: u32,
    mode: ReceiveMode,
    use_tags: bool,
    proposer_plan: SymmetryPlan,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
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

#[derive(Clone)]
struct PlanJob {
    plan: SymmetryPlan,
}

#[derive(Default)]
struct AggregatedStats {
    execs: usize,
    block: usize,
    plans: usize,
}

fn run_protocol_worker(
    num_nodes: usize,
    num_ballots: u32,
    mode: ReceiveMode,
    use_tags: bool,
    workers: usize,
) -> AggregatedStats {
    let requested_workers = workers.max(1);

    let mut plans = 0usize;
    let mut count_plan = |_plan: SymmetryPlan| {
        plans += 1;
        true
    };
    let _ = enumerate_symmetry_reduced_plans(num_nodes, num_ballots, &mut count_plan);
    println!("Plans (to run) = {}", plans);
    if plans == 0 {
        println!(
            "Workers requested = {}, using = 0 (no plans)",
            requested_workers
        );
        let _ = std::io::stdout().flush();
        return AggregatedStats::default();
    }
    let workers = requested_workers.min(plans);
    println!("Workers requested = {}, using = {}", requested_workers, workers);
    let _ = std::io::stdout().flush();

    let (job_tx, job_rx) = mpsc::sync_channel::<PlanJob>(workers * 2);
    let (stats_tx, stats_rx) = mpsc::channel::<traceforge::Stats>();
    let job_rx = Arc::new(Mutex::new(job_rx));

    let mut worker_handles = Vec::new();
    for _ in 0..workers {
        let rx = Arc::clone(&job_rx);
        let tx = stats_tx.clone();
        worker_handles.push(std::thread::spawn(move || loop {
            let job = {
                let guard = rx.lock().expect("job queue mutex poisoned");
                guard.recv()
            };
            let job = match job {
                Ok(job) => job,
                Err(_) => break,
            };
            let stats = run_protocol_for_plan(num_nodes, num_ballots, mode, use_tags, job.plan);
            tx.send(stats).expect("failed to send worker stats");
        }));
    }
    drop(stats_tx);

    let mut send_plan = |plan: SymmetryPlan| {
        job_tx.send(PlanJob { plan }).expect("failed to submit plan");
        true
    };
    let _ = enumerate_symmetry_reduced_plans(num_nodes, num_ballots, &mut send_plan);
    drop(job_tx);

    let mut out = AggregatedStats {
        plans,
        ..AggregatedStats::default()
    };
    for _ in 0..plans {
        let stats = stats_rx.recv().expect("worker terminated before sending stats");
        out.execs += stats.execs;
        out.block += stats.block;
    }

    for handle in worker_handles {
        handle.join().expect("worker panicked");
    }
    out
}

fn parse_args() -> (usize, u32, ReceiveMode, bool, usize) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_ballots = DEFAULT_NUM_BALLOTS;
    let mut mode = DEFAULT_MODE;
    let mut use_tags = DEFAULT_USE_TAGS;
    let mut workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
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
            "--workers" => {
                let value = next_arg_value(&mut args, "--workers");
                workers = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --workers value: {}", value));
            }
            _ => {
                panic!(
                    "unknown argument: {} (expected --nodes <n>, --ballots <n>, --mode <recv|inbox>, --wo-tags, --workers <n>)",
                    arg
                );
            }
        }
    }

    (num_nodes, num_ballots, mode, use_tags, workers)
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
    let (num_nodes, num_ballots, mode, use_tags, workers) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol_worker(num_nodes, num_ballots, mode, use_tags, workers);
    println!("Plans = {}", stats.plans);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
