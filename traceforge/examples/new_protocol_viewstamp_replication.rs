use traceforge::new_comm_close::{MatchKind, RoundFilter, RoundStamp, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_MAX_VIEWS: u32 = 1;
const DEFAULT_MAX_OPS: u32 = 1;
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
enum Kind {
    ViewChange,
    Normal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum VcRound {
    DoViewChange,
    StartView,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum OpRound {
    Prepare,
    PrepareOk,
}

#[derive(Clone, traceforge::Round)]
struct ViewstampRound {
    #[dimension("=")]
    view: u32,
    #[dimension("=")]
    kind: Kind,
    #[dimension("=")]
    vc_round: VcRound,
    #[dimension("=")]
    op: u32,
    #[dimension("=")]
    op_round: OpRound,
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

    fn primary_for_view(&self, view: u32) -> ThreadId {
        let idx = (view as usize) % self.nodes.len();
        self.nodes[idx]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogEntry {
    value: u32,
    committed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    DoViewChange {
        view: u32,
        sender: ThreadId,
        log: Vec<LogEntry>,
    },
    StartView {
        view: u32,
        sender: ThreadId,
        log: Vec<LogEntry>,
    },
    Prepare {
        view: u32,
        op: u32,
        value: u32,
        sender: ThreadId,
        commit_hint: Option<(u32, u32)>,
    },
    PrepareOk {
        view: u32,
        op: u32,
        sender: ThreadId,
    },
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds<ViewstampRound>,
    mode: ReceiveMode,
    log: Vec<LogEntry>,
    max_views: u32,
    max_ops: u32,
    started: bool,
}

impl Node {
    fn new(
        nodes: Participants,
        max_views: u32,
        max_ops: u32,
        mode: ReceiveMode,
        use_tags: bool,
    ) -> Self {
        let me = thread::current().id();
        let rounds = if use_tags {
            Rounds::<ViewstampRound>::new()
        } else {
            Rounds::<ViewstampRound>::new_wo_tags(false)
        };
        Self {
            nodes,
            me,
            rounds,
            mode,
            log: Vec::new(),
            max_views,
            max_ops,
            started: false,
        }
    }

    fn run(mut self) -> Vec<LogEntry> {
        while self.rounds.current().view() < self.max_views {
            if !self.do_view_change() {
                continue;
            }
            if self.do_normal_ops() {
                break;
            }
        }
        self.log
    }

    fn do_view_change(&mut self) -> bool {
        let view_round = self.next_view_round();
        let view = view_round.view();
        let primary = self.nodes.primary_for_view(view);
        let quorum = self.nodes.len() / 2 + 1;

        self.rounds.send(
            primary,
            Message::DoViewChange {
                view,
                sender: self.me,
                log: self.log.clone(),
            },
        );

        if self.me == primary {
            let mbox = self.collect_do_view_change(0, self.nodes.len());
            if mbox.len() < quorum {
                self.rounds.advance(ViewstampRound::view());
                return false;
            }

            let mut best = self.log.clone();
            for (msg, _) in &mbox {
                if let Message::DoViewChange { log, .. } = msg {
                    if Self::better_log(log, &best) {
                        best = log.clone();
                    }
                }
            }
            self.log = best;

            self.rounds.advance(ViewstampRound::vc_round());
            self.broadcast(Message::StartView {
                view,
                sender: self.me,
                log: self.log.clone(),
            });

            self.rounds.advance(ViewstampRound::kind());
            self.rounds
                .advance_to(ViewstampRound::op(), self.log.len() as u32);
            true
        } else {
            self.rounds.advance(ViewstampRound::vc_round());
            let mbox = self.collect_start_view(0, 1);
            if mbox.len() != 1 {
                self.rounds.advance(ViewstampRound::view());
                return false;
            }

            let (msg, stamp) = &mbox[0];
            self.rounds.jump(stamp);
            match msg {
                Message::StartView { log, .. } => {
                    self.log = log.clone();
                }
                _ => panic!("expected StartView"),
            }

            self.rounds.advance(ViewstampRound::kind());
            self.rounds
                .advance_to(ViewstampRound::op(), self.log.len() as u32);
            true
        }
    }

    fn do_normal_ops(&mut self) -> bool {
        let mut stable = true;
        let view = self.rounds.current().view();
        let primary = self.nodes.primary_for_view(view);
        let quorum = self.nodes.len() / 2 + 1;

        for _ in 0..self.max_ops {
            let round = self.rounds.current();
            let op = round.op();

            if self.me == primary {
                let value = (0..=MAX_VALUE as usize).nondet() as u32;
                self.ensure_op_value(op, value);
                let commit_hint = self.last_committed_pair();

                self.broadcast(Message::Prepare {
                    view,
                    op,
                    value,
                    sender: self.me,
                    commit_hint,
                });

                self.rounds.advance(ViewstampRound::op_round());
                self.rounds.send(
                    primary,
                    Message::PrepareOk {
                        view,
                        op,
                        sender: self.me,
                    },
                );

                let oks = self.collect_prepare_ok(0, self.nodes.len());
                if oks.len() >= quorum {
                    self.mark_committed(op);
                    self.rounds.advance(ViewstampRound::op());
                } else {
                    stable = false;
                    self.rounds.advance(ViewstampRound::view());
                    break;
                }
            } else {
                let prepares = self.collect_prepare(0, self.nodes.len());
                let chosen = prepares.into_iter().find(
                    |(msg, _)| matches!(msg, Message::Prepare { sender, .. } if *sender == primary),
                );

                let Some((msg, stamp)) = chosen else {
                    stable = false;
                    self.rounds.advance(ViewstampRound::view());
                    break;
                };

                let (op, value, commit_hint) = match msg {
                    Message::Prepare {
                        op,
                        value,
                        commit_hint,
                        ..
                    } => (op, value, commit_hint),
                    _ => panic!("expected Prepare"),
                };

                self.rounds.jump(&stamp);
                self.ensure_op_value(op, value);
                if let Some((committed_op, committed_value)) = commit_hint {
                    self.ensure_op_value(committed_op, committed_value);
                    self.mark_committed(committed_op);
                }

                self.rounds.advance(ViewstampRound::op_round());
                self.rounds.send(
                    primary,
                    Message::PrepareOk {
                        view,
                        op,
                        sender: self.me,
                    },
                );
                self.rounds.advance(ViewstampRound::op());
            }
        }

        stable
    }

    fn next_view_round(&mut self) -> traceforge::new_comm_close::Round<ViewstampRound> {
        if !self.started {
            self.started = true;
        }
        self.rounds.current()
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            self.rounds.send(*node, msg.clone());
        }
    }

    fn ensure_op_value(&mut self, op: u32, value: u32) {
        let idx = op as usize;
        while self.log.len() <= idx {
            self.log.push(LogEntry {
                value: 0,
                committed: false,
            });
        }
        self.log[idx].value = value;
    }

    fn mark_committed(&mut self, op: u32) {
        let idx = op as usize;
        if idx < self.log.len() {
            self.log[idx].committed = true;
        }
    }

    fn last_committed_pair(&self) -> Option<(u32, u32)> {
        for (i, entry) in self.log.iter().enumerate().rev() {
            if entry.committed {
                return Some((i as u32, entry.value));
            }
        }
        None
    }

    fn better_log(candidate: &[LogEntry], current: &[LogEntry]) -> bool {
        let cand_committed = candidate.iter().filter(|entry| entry.committed).count();
        let curr_committed = current.iter().filter(|entry| entry.committed).count();
        if cand_committed != curr_committed {
            return cand_committed > curr_committed;
        }
        candidate.len() > current.len()
    }

    fn collect_do_view_change(&self, min: usize, max: usize) -> Vec<(Message, RoundStamp<ViewstampRound>)> {
        let filter = self
            .rounds
            .filter()
            .view(MatchKind::Eq)
            .kind(MatchKind::Eq)
            .vc_round(MatchKind::Eq);
        self.collect_messages_with_filter(&filter, min, max)
            .into_iter()
            .filter(|(msg, _)| matches!(msg, Message::DoViewChange { .. }))
            .collect()
    }

    fn collect_start_view(&self, min: usize, max: usize) -> Vec<(Message, RoundStamp<ViewstampRound>)> {
        let filter = self
            .rounds
            .filter()
            .view(MatchKind::Eq)
            .kind(MatchKind::Eq)
            .vc_round(MatchKind::Eq);
        self.collect_messages_with_filter(&filter, min, max)
            .into_iter()
            .filter(|(msg, _)| matches!(msg, Message::StartView { .. }))
            .collect()
    }

    fn collect_prepare(&self, min: usize, max: usize) -> Vec<(Message, RoundStamp<ViewstampRound>)> {
        let filter = self
            .rounds
            .filter()
            .view(MatchKind::Eq)
            .kind(MatchKind::Eq)
            .op(MatchKind::Gte)
            .op_round(MatchKind::Eq);
        self.collect_messages_with_filter(&filter, min, max)
            .into_iter()
            .filter(|(msg, _)| matches!(msg, Message::Prepare { .. }))
            .collect()
    }

    fn collect_prepare_ok(&self, min: usize, max: usize) -> Vec<Message> {
        let filter = self
            .rounds
            .filter()
            .view(MatchKind::Eq)
            .kind(MatchKind::Eq)
            .op(MatchKind::Eq)
            .op_round(MatchKind::Eq);
        self.collect_messages_with_filter(&filter, min, max)
            .into_iter()
            .map(|(msg, _)| msg)
            .filter(|msg| matches!(msg, Message::PrepareOk { .. }))
            .collect()
    }

    fn collect_messages_with_filter(
        &self,
        filter: &RoundFilter<ViewstampRound>,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp<ViewstampRound>)> {
        assert!(max >= min, "requires max >= min");
        match self.mode {
            ReceiveMode::Recv => {
                let mut out = Vec::new();
                let count = if max == min {
                    min
                } else {
                    (min..=max).nondet()
                };
                for _ in 0..count {
                    let msg = self.rounds.recv_block_with::<Message>(filter);
                    out.push((msg.payload().clone(), msg.stamp().clone()));
                }
                out
            }
            ReceiveMode::Inbox => self
                .rounds
                .inbox_with_bounds_with::<Message>(filter, min, Some(max))
                .into_iter()
                .flatten()
                .map(|msg| (msg.payload().clone(), msg.stamp().clone()))
                .collect(),
        }
    }
}

fn start_node(max_views: u32, max_ops: u32, mode: ReceiveMode, use_tags: bool) -> Vec<LogEntry> {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, max_views, max_ops, mode, use_tags).run()
}

fn assert_committed_consistency(logs: &[Vec<LogEntry>]) {
    let max_len = logs.iter().map(|log| log.len()).max().unwrap_or(0);
    for idx in 0..max_len {
        let mut chosen: Option<u32> = None;
        for log in logs {
            if let Some(entry) = log.get(idx) {
                if entry.committed {
                    if let Some(prev) = chosen {
                        assert_eq!(prev, entry.value);
                    } else {
                        chosen = Some(entry.value);
                    }
                }
            }
        }
    }
}

fn run_protocol(
    num_nodes: usize,
    max_views: u32,
    max_ops: u32,
    mode: ReceiveMode,
    use_tags: bool,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            handles.push(thread::spawn(move || {
                start_node(max_views, max_ops, mode, use_tags)
            }));
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

fn parse_args() -> (usize, u32, u32, ReceiveMode, bool) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut views = DEFAULT_MAX_VIEWS;
    let mut ops = DEFAULT_MAX_OPS;
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
            "--views" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| panic!("--views requires a value"));
                views = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --views value: {}", value));
            }
            "--ops" => {
                let value = args.next().unwrap_or_else(|| panic!("--ops requires a value"));
                ops = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --ops value: {}", value));
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
                    "unknown argument: {} (expected --nodes <n>, --views <n>, --ops <n>, --mode <recv|inbox>, --wo-tags)",
                    arg
                );
            }
        }
    }

    (num_nodes, views, ops, mode, use_tags)
}

fn main() {
    let (num_nodes, views, ops, mode, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_nodes, views, ops, mode, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
