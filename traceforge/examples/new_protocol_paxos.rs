use traceforge::new_comm_close::{MatchKind, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum Phase {
    Prepare,
    Promise,
    Accept,
    Accepted,
}

#[derive(Clone, traceforge::Round)]
struct PaxosRound {
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
    rounds: Rounds<PaxosRound>,
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
        let me = thread::current().id();
        let rounds = if use_tags {
            Rounds::<PaxosRound>::new()
        } else {
            Rounds::<PaxosRound>::new_wo_tags(false)
        };
        Self {
            nodes,
            me,
            rounds,
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

        // Phase 1: Prepare
        let i_propose = traceforge::nondet() && self.decided_value.is_none();
        if i_propose {
            self.broadcast(Message::Prepare(PrepareMsg {
                ballot,
                proposer: self.me,
                sender: self.me,
            }));
        }
        self.handle_prepare_messages();

        // Phase 2: Promise
        self.rounds.advance(PaxosRound::phase());
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

        // Phase 3: Accept
        self.rounds.advance(PaxosRound::phase());
        self.handle_accept_requests();

        // Phase 4: Accepted
        self.rounds.advance(PaxosRound::phase());
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

    fn enter_next_ballot(&mut self) -> traceforge::new_comm_close::Round<PaxosRound> {
        if self.started {
            self.rounds.advance(PaxosRound::ballot())
        } else {
            self.started = true;
            self.rounds.current()
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
                self.rounds.send(
                    prepare.proposer,
                    Message::Promise(PromiseMsg {
                        ballot: prepare.ballot,
                        proposer: prepare.proposer,
                        sender: self.me,
                        accepted_ballot: self.accepted_ballot,
                        accepted_value: self.accepted_value,
                    }),
                );
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
                self.rounds.send(
                    req.proposer,
                    Message::Accepted(AcceptedMsg {
                        ballot: req.ballot,
                        proposer: req.proposer,
                        value: req.value,
                        sender: self.me,
                    }),
                );
            }
        }
    }

    fn collect_promises_for(&self, ballot: u32, proposer: ThreadId, max: usize) -> Vec<PromiseMsg> {
        self.collect_phase_messages(max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Promise(p) if p.ballot == ballot && p.proposer == proposer => Some(p),
                _ => None,
            })
            .collect()
    }

    fn collect_accepted_for(&self, ballot: u32, proposer: ThreadId, max: usize) -> Vec<AcceptedMsg> {
        self.collect_phase_messages(max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Accepted(a) if a.ballot == ballot && a.proposer == proposer => Some(a),
                _ => None,
            })
            .collect()
    }

    fn collect_phase_messages(&self, max: usize) -> Vec<Message> {
        let filter = self
            .rounds
            .filter()
            .ballot(MatchKind::Eq)
            .phase(MatchKind::Eq);
        match self.mode {
            ReceiveMode::Recv => {
                let count = (0..=max).nondet();
                let mut out = Vec::new();
                for _ in 0..count {
                    let msg = self.rounds.recv_block_with::<Message>(&filter);
                    out.push(msg.payload().clone());
                }
                out
            }
            ReceiveMode::Inbox => self
                .rounds
                .inbox_with_bounds_with::<Message>(&filter, 0, Some(max))
                .into_iter()
                .flatten()
                .map(|msg| msg.payload().clone())
                .collect(),
        }
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            self.rounds.send(*node, msg.clone());
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
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            handles.push(thread::spawn(move || start_node(num_ballots, mode, use_tags)));
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
        assert_decision_consistency(&logs);
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
