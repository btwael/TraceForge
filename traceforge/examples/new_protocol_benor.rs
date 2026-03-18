use traceforge::comm_close::{MatchKind, RoundFilter, Rounds};
use traceforge::thread::ThreadId;
use traceforge::thread;
use traceforge::Nondet;

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_ROUNDS: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Recv;
const DEFAULT_USE_TAGS: bool = true;
const INIT_TAG: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum Phase {
    Vote1,
    Vote2,
}

#[derive(Clone, traceforge::Round)]
struct BenOrRound {
    #[dimension("=")]
    round: u32,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bit {
    Zero,
    One,
}

impl Bit {
    fn random() -> Self {
        if traceforge::nondet() {
            Bit::Zero
        } else {
            Bit::One
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    Vote1 {
        round: u32,
        value: Bit,
        sender: ThreadId,
    },
    Vote2 {
        round: u32,
        value: Option<Bit>,
        sender: ThreadId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NodeLog {
    decision: Option<Bit>,
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds<BenOrRound>,
    mode: ReceiveMode,
    max_rounds: u32,
    started: bool,
    estimate: Bit,
    decided: Option<Bit>,
}

impl Node {
    fn new(nodes: Participants, max_rounds: u32, mode: ReceiveMode, use_tags: bool) -> Self {
        let me = thread::current().id();
        let rounds = if use_tags {
            Rounds::<BenOrRound>::new()
        } else {
            Rounds::<BenOrRound>::new_wo_tags(false)
        };
        Self {
            nodes,
            me,
            rounds,
            mode,
            max_rounds,
            started: false,
            estimate: Bit::random(),
            decided: None,
        }
    }

    fn run(mut self) -> NodeLog {
        for _ in 0..self.max_rounds {
            self.step_round();
        }
        NodeLog {
            decision: self.decided,
        }
    }

    fn step_round(&mut self) {
        let round = self.next_round().round();

        // Phase 1: broadcast current estimate and collect all peers' Vote1.
        self.broadcast(Message::Vote1 {
            round,
            value: self.estimate,
            sender: self.me,
        });

        let vote1 = self.collect_vote1(round, self.nodes.len().saturating_sub(1));
        let mut count_zero = usize::from(self.estimate == Bit::Zero);
        let mut count_one = usize::from(self.estimate == Bit::One);
        for v in vote1 {
            match v {
                Bit::Zero => count_zero += 1,
                Bit::One => count_one += 1,
            }
        }

        let strong = if count_zero > self.nodes.len() / 2 {
            Some(Bit::Zero)
        } else if count_one > self.nodes.len() / 2 {
            Some(Bit::One)
        } else {
            None
        };

        self.rounds.advance(BenOrRound::phase());

        // Phase 2: broadcast support if strong majority seen, else abstain.
        self.broadcast(Message::Vote2 {
            round,
            value: strong,
            sender: self.me,
        });

        let vote2 = self.collect_vote2(round, self.nodes.len().saturating_sub(1));
        let mut support_zero = usize::from(strong == Some(Bit::Zero));
        let mut support_one = usize::from(strong == Some(Bit::One));
        for vote in vote2 {
            match vote {
                Some(Bit::Zero) => support_zero += 1,
                Some(Bit::One) => support_one += 1,
                None => {}
            }
        }

        if support_zero > self.nodes.len() / 2 {
            self.decided = Some(Bit::Zero);
            self.estimate = Bit::Zero;
        } else if support_one > self.nodes.len() / 2 {
            self.decided = Some(Bit::One);
            self.estimate = Bit::One;
        } else if support_zero > 0 {
            self.estimate = Bit::Zero;
        } else if support_one > 0 {
            self.estimate = Bit::One;
        } else {
            self.estimate = Bit::random();
        }
    }

    fn next_round(&mut self) -> traceforge::comm_close::Round<BenOrRound> {
        if self.started {
            self.rounds.advance(BenOrRound::round())
        } else {
            self.started = true;
            self.rounds.current()
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

    fn collect_vote1(&self, round: u32, expected: usize) -> Vec<Bit> {
        let filter = self.vote1_filter();
        let mut out = Vec::new();
        for msg in self.collect_messages(&filter, expected) {
            match msg.payload() {
                Message::Vote1 {
                    round: msg_round,
                    value,
                    ..
                } => {
                    assert_eq!(*msg_round, round);
                    out.push(*value);
                }
                _ => panic!("expected Vote1"),
            }
        }
        out
    }

    fn collect_vote2(&self, round: u32, expected: usize) -> Vec<Option<Bit>> {
        let filter = self.vote2_filter();
        let mut out = Vec::new();
        for msg in self.collect_messages(&filter, expected) {
            match msg.payload() {
                Message::Vote2 {
                    round: msg_round,
                    value,
                    ..
                } => {
                    assert_eq!(*msg_round, round);
                    out.push(*value);
                }
                _ => panic!("expected Vote2"),
            }
        }
        out
    }

    fn vote1_filter(&self) -> RoundFilter<BenOrRound> {
        self.rounds
            .filter()
            .round(MatchKind::Eq)
            .phase(MatchKind::Eq)
    }

    fn vote2_filter(&self) -> RoundFilter<BenOrRound> {
        self.rounds
            .filter()
            .round(MatchKind::Eq)
            .phase(MatchKind::Eq)
    }

    fn collect_messages(
        &self,
        filter: &RoundFilter<BenOrRound>,
        max_expected: usize,
    ) -> Vec<traceforge::comm_close::RoundMsg<Message, BenOrRound>> {
        match self.mode {
            ReceiveMode::Recv => {
                let mut out = Vec::new();
                let count = if max_expected == 0 {
                    0
                } else {
                    (0..=max_expected).nondet()
                };
                for _ in 0..count {
                    out.push(self.rounds.recv_block_with::<Message>(filter));
                }
                out
            }
            ReceiveMode::Inbox => {
                self.rounds
                    .inbox_with_bounds_with::<Message>(filter, 0, Some(max_expected))
                    .into_iter()
                    .flatten()
                    .collect()
            }
        }
    }
}

fn start_node(max_rounds: u32, mode: ReceiveMode, use_tags: bool) -> NodeLog {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, max_rounds, mode, use_tags).run()
}

fn assert_decision_consistency(logs: &[NodeLog]) {
    let mut decided: Option<Bit> = None;
    for log in logs {
        if let Some(v) = log.decision {
            if let Some(prev) = decided {
                assert_eq!(prev, v);
            } else {
                decided = Some(v);
            }
        }
    }
}

fn run_protocol(
    num_nodes: usize,
    max_rounds: u32,
    mode: ReceiveMode,
    use_tags: bool,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            handles.push(thread::spawn(move || start_node(max_rounds, mode, use_tags)));
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

fn parse_mode(value: &str) -> ReceiveMode {
    match value {
        "recv" => ReceiveMode::Recv,
        "inbox" => ReceiveMode::Inbox,
        _ => panic!("invalid --mode value: {} (expected recv or inbox)", value),
    }
}

fn parse_args() -> (usize, u32, ReceiveMode, bool) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut rounds = DEFAULT_NUM_ROUNDS;
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
            "--rounds" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| panic!("--rounds requires a value"));
                rounds = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --rounds value: {}", value));
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
                    "unknown argument: {} (expected --nodes <n>, --rounds <n>, --mode <recv|inbox>, --wo-tags)",
                    arg
                );
            }
        }
    }

    (num_nodes, rounds, mode, use_tags)
}

fn main() {
    let (num_nodes, rounds, mode, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, rounds, mode, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
