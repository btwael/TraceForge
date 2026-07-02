use traceforge::comm_close::{self, TraceForgeTransportMode};
use traceforge::thread;
use traceforge::thread::ThreadId;
use traceforge::{BranchingStrategy, Nondet};
use traceforge_rounds::{Comm, Dim, Round};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
enum Kind {
    ViewChange,
    Normal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
enum VcRound {
    DoViewChange,
    StartView,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
enum OpRound {
    Prepare,
    PrepareOk,
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
struct ViewstampRound {
    view: u32,
    kind: Kind,
    vc_round: VcRound,
    op: u32,
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
    comm: Comm<ViewstampRound, comm_close::TraceForgeTransport>,
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
        let me = thread::current_id();
        let comm = comm_close::comm_with::<ViewstampRound>(transport_mode(mode, use_tags));
        Self {
            nodes,
            me,
            comm,
            mode,
            log: Vec::new(),
            max_views,
            max_ops,
            started: false,
        }
    }

    fn run(mut self) -> Vec<LogEntry> {
        while self.comm.rounds().current().view() < self.max_views {
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
        let view = self.next_view_round().view();
        let primary = self.nodes.primary_for_view(view);
        let quorum = self.nodes.len() / 2 + 1;

        self.comm
            .send(
                primary,
                Message::DoViewChange {
                    view,
                    sender: self.me,
                    log: self.log.clone(),
                },
            )
            .unwrap();

        if self.me == primary {
            let mbox = self.collect_do_view_change(0, self.nodes.len());
            if mbox.len() < quorum {
                self.comm.rounds().advance(ViewstampRound::dim_view());
                return false;
            }

            let mut best = self.log.clone();
            for (_, msg) in &mbox {
                if let Message::DoViewChange { log, .. } = msg {
                    if Self::better_log(log, &best) {
                        best = log.clone();
                    }
                }
            }
            self.log = best;

            self.comm.rounds().advance(ViewstampRound::dim_vc_round());
            self.broadcast(Message::StartView {
                view,
                sender: self.me,
                log: self.log.clone(),
            });

            self.comm.rounds().advance(ViewstampRound::dim_kind());
            self.comm
                .rounds()
                .advance_to_op(self.log.len() as u32)
                .unwrap();
            true
        } else {
            self.comm.rounds().advance(ViewstampRound::dim_vc_round());
            let mbox = self.collect_start_view(0, 1);
            if mbox.len() != 1 {
                self.comm.rounds().advance(ViewstampRound::dim_view());
                return false;
            }

            let (stamp, msg) = &mbox[0];
            self.comm.rounds().jump(stamp.clone()).unwrap();
            match msg {
                Message::StartView { log, .. } => {
                    self.log = log.clone();
                }
                _ => panic!("expected StartView"),
            }

            self.comm.rounds().advance(ViewstampRound::dim_kind());
            self.comm
                .rounds()
                .advance_to_op(self.log.len() as u32)
                .unwrap();
            true
        }
    }

    fn do_normal_ops(&mut self) -> bool {
        let mut stable = true;
        let view = self.comm.rounds().current().view();
        let primary = self.nodes.primary_for_view(view);
        let quorum = self.nodes.len() / 2 + 1;

        for _ in 0..self.max_ops {
            let op = self.comm.rounds().current().op();

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

                self.comm.rounds().advance(ViewstampRound::dim_op_round());
                self.comm
                    .send(
                        primary,
                        Message::PrepareOk {
                            view,
                            op,
                            sender: self.me,
                        },
                    )
                    .unwrap();

                let oks = self.collect_prepare_ok(0, self.nodes.len());
                if oks.len() >= quorum {
                    self.mark_committed(op);
                    self.comm.rounds().advance(ViewstampRound::dim_op());
                } else {
                    stable = false;
                    self.comm.rounds().advance(ViewstampRound::dim_view());
                    break;
                }
            } else {
                let prepares = self.collect_prepare(0, self.nodes.len());
                let chosen = prepares.into_iter().find(
                    |(_, msg)| matches!(msg, Message::Prepare { sender, .. } if *sender == primary),
                );

                let Some((stamp, msg)) = chosen else {
                    stable = false;
                    self.comm.rounds().advance(ViewstampRound::dim_view());
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

                self.comm.rounds().jump(stamp).unwrap();
                self.ensure_op_value(op, value);
                if let Some((committed_op, committed_value)) = commit_hint {
                    self.ensure_op_value(committed_op, committed_value);
                    self.mark_committed(committed_op);
                }

                self.comm.rounds().advance(ViewstampRound::dim_op_round());
                self.comm
                    .send(
                        primary,
                        Message::PrepareOk {
                            view,
                            op,
                            sender: self.me,
                        },
                    )
                    .unwrap();
                self.comm.rounds().advance(ViewstampRound::dim_op());
            }
        }

        stable
    }

    fn next_view_round(&mut self) -> &ViewstampRound {
        if !self.started {
            self.started = true;
        }
        self.comm.rounds().current()
    }

    fn broadcast(&mut self, msg: Message) {
        for node in self.nodes.iter() {
            self.comm.send(*node, msg.clone()).unwrap();
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

    fn collect_do_view_change(&mut self, min: usize, max: usize) -> Vec<(ViewstampRound, Message)> {
        self.collect_messages_with_filter(same_view_change_filter, min, max)
            .into_iter()
            .filter(|(_, msg)| matches!(msg, Message::DoViewChange { .. }))
            .collect()
    }

    fn collect_start_view(&mut self, min: usize, max: usize) -> Vec<(ViewstampRound, Message)> {
        self.collect_messages_with_filter(same_view_change_filter, min, max)
            .into_iter()
            .filter(|(_, msg)| matches!(msg, Message::StartView { .. }))
            .collect()
    }

    fn collect_prepare(&mut self, min: usize, max: usize) -> Vec<(ViewstampRound, Message)> {
        self.collect_messages_with_filter(prepare_filter, min, max)
            .into_iter()
            .filter(|(_, msg)| matches!(msg, Message::Prepare { .. }))
            .collect()
    }

    fn collect_prepare_ok(&mut self, min: usize, max: usize) -> Vec<Message> {
        self.collect_messages_with_filter(same_normal_op_filter, min, max)
            .into_iter()
            .map(|(_, msg)| msg)
            .filter(|msg| matches!(msg, Message::PrepareOk { .. }))
            .collect()
    }

    fn collect_messages_with_filter(
        &mut self,
        filter: fn(&ViewstampRound, &ViewstampRound) -> bool,
        min: usize,
        max: usize,
    ) -> Vec<(ViewstampRound, Message)> {
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
                    out.push(
                        self.comm
                            .recv_block_stamped_with::<Message, _>(filter)
                            .unwrap(),
                    );
                }
                out
            }
            ReceiveMode::Inbox => self
                .comm
                .inbox_stamped_with_bounds_with::<Message, _>(min, Some(max), filter)
                .unwrap()
                .into_iter()
                .flatten()
                .collect(),
        }
    }
}

fn same_view_change_filter(local: &ViewstampRound, remote: &ViewstampRound) -> bool {
    local.view() == remote.view()
        && local.kind() == remote.kind()
        && local.vc_round() == remote.vc_round()
}

fn prepare_filter(local: &ViewstampRound, remote: &ViewstampRound) -> bool {
    local.view() == remote.view()
        && local.kind() == remote.kind()
        && remote.op() >= local.op()
        && local.op_round() == remote.op_round()
}

fn same_normal_op_filter(local: &ViewstampRound, remote: &ViewstampRound) -> bool {
    local.view() == remote.view()
        && local.kind() == remote.kind()
        && local.op() == remote.op()
        && local.op_round() == remote.op_round()
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
                .with_partitioned_branching(BranchingStrategy::RevisitQueueRayon)
                .with_iterations_until_split(5000);
        }
    }

    traceforge::verify(config.build(), move || {
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
        assert_committed_consistency(&logs);
    })
}

fn transport_mode(mode: ReceiveMode, use_tags: bool) -> TraceForgeTransportMode {
    match (mode, use_tags) {
        (ReceiveMode::Inbox, true) => TraceForgeTransportMode::TaggedNativeInbox,
        (ReceiveMode::Recv, true) => TraceForgeTransportMode::TaggedRepeatedRecv,
        (ReceiveMode::Recv, false) => TraceForgeTransportMode::UntaggedRepeatedRecv,
        (ReceiveMode::Inbox, false) => {
            panic!("--wo-tags is only supported with --mode rounds/recv");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParallelMode {
    Sequential,
    Shared(usize),
    Rayon(usize),
}

fn parse_args() -> (usize, u32, u32, ReceiveMode, bool, ParallelMode) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut views = DEFAULT_MAX_VIEWS;
    let mut ops = DEFAULT_MAX_OPS;
    let mut mode = DEFAULT_MODE;
    let mut use_tags = DEFAULT_USE_TAGS;
    let mut explicit_wo_tags = false;
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
            "--views" => {
                let value = next_arg_value(&mut args, "--views");
                views = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --views value: {value}"));
            }
            "--ops" => {
                let value = next_arg_value(&mut args, "--ops");
                ops = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --ops value: {value}"));
            }
            "--mode" => {
                let value = next_arg_value(&mut args, "--mode");
                let parsed = parse_mode(&value);
                mode = parsed.0;
                use_tags = parsed.1 && !explicit_wo_tags;
            }
            "--wo-tags" => {
                explicit_wo_tags = true;
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
                    "unknown argument: {arg} (expected --nodes <n>, --views <n>, --ops <n>, --mode <full|rounds|dpor>, --wo-tags, --parallel <n>, --rayon <n>)",
                );
            }
        }
    }

    (num_nodes, views, ops, mode, use_tags, parallel)
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

fn parse_mode(value: &str) -> (ReceiveMode, bool) {
    match value {
        "full" | "inbox" => (ReceiveMode::Inbox, true),
        "rounds" | "recv" => (ReceiveMode::Recv, true),
        "dpor" => (ReceiveMode::Recv, false),
        _ => panic!("invalid --mode value: {value} (expected full, rounds, dpor, inbox, or recv)"),
    }
}

fn main() {
    let (num_nodes, views, ops, mode, use_tags, parallel) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode rounds/recv");
    }
    let stats = run_protocol(num_nodes, views, ops, mode, use_tags, parallel);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
