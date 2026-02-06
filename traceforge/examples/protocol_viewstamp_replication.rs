
use traceforge::comm_close::{self, DefaultMatch, RoundScheme, RoundStamp, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_MAX_VIEWS: u32 = 1;
const DEFAULT_MAX_OPS: u32 = 1;
const MAX_VALUE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundKey)]
enum Key {
    View,
    Kind,
    VcRound,
    Op,
    OpRound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum Kind {
    ViewChange,
    Normal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum VcRound {
    DoViewChange,
    StartView,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum OpRound {
    Prepare,
    PrepareOk,
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

    // View change.
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

    // Normal operation.
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
    rounds: Rounds,
    mode: ReceiveMode,

    // Replicated log.
    log: Vec<LogEntry>,

    max_views: u32,
    max_ops: u32,

    started: bool,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme, max_views: u32, max_ops: u32, mode: ReceiveMode) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
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
        // As in the C spec: we keep trying view changes until one view is established.
        // Then we run a bounded number of normal operations, unless we trigger a view change again.
        while self.rounds.current().get_u32(Key::View) < self.max_views {
            if !self.do_view_change() {
                continue;
            }
            if self.do_normal_ops() {
                break;
            }
            // do_normal_ops may advance the view on failure; retry.
        }
        self.log
    }

    /// Executes one view-change attempt. Returns true if the view is established and we enter normal mode.
    fn do_view_change(&mut self) -> bool {
        let view_round = self.next_view_round();
        let view = view_round.get_u32(Key::View);
        let primary = self.nodes.primary_for_view(view);
        let quorum = self.nodes.len() / 2 + 1;

        // DoViewChange: everyone sends their log to the new primary.
        comm_close::send(
            primary,
            Message::DoViewChange {
                view,
                sender: self.me,
                log: self.log.clone(),
            },
        );

        if self.me == primary {
            // Primary collects DoViewChange messages.
            let do_vc_round = self.rounds.current();
            let mbox = self.collect_do_view_change(&do_vc_round, 0, self.nodes.len());

            if mbox.len() < quorum {
                // Timeout -> next view.
                self.rounds.advance(Key::View);
                return false;
            }

            // Choose the log with the most committed entries (tie-breaker: longest).
            let mut best = self.log.clone();
            for (m, _) in &mbox {
                if let Message::DoViewChange { log, .. } = m {
                    if Self::better_log(log, &best) {
                        best = log.clone();
                    }
                }
            }
            self.log = best;

            // StartView: broadcast chosen log.
            let _start_view_round = self.rounds.advance(Key::VcRound);
            self.broadcast(Message::StartView {
                view,
                sender: self.me,
                log: self.log.clone(),
            });

            // Switch to Normal mode.
            let _normal_round = self.rounds.advance(Key::Kind);
            self.rounds.goto_u32(Key::Op, self.log.len() as u32);
            true
        } else {
            // Backup waits for StartView.
            let _start_view_round = self.rounds.advance(Key::VcRound);
            let mbox = self.collect_start_view(&self.rounds.current(), 0, 1);
            if mbox.len() != 1 {
                self.rounds.advance(Key::View);
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

            let _normal_round = self.rounds.advance(Key::Kind);
            self.rounds.goto_u32(Key::Op, self.log.len() as u32);
            true
        }
    }

    fn do_normal_ops(&mut self) -> bool {
        let mut stable = true;
        let view = self.rounds.current().get_u32(Key::View);
        let primary = self.nodes.primary_for_view(view);
        let quorum = self.nodes.len() / 2 + 1;

        for _ in 0..self.max_ops {
            let r = self.rounds.current();
            let op = r.get_u32(Key::Op);

            if self.me == primary {
                // Prepare: propose a new operation value.
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

                // PrepareOk: collect a quorum.
                let _ok_round = self.rounds.advance(Key::OpRound);

                // Count leader itself.
                comm_close::send(
                    primary,
                    Message::PrepareOk {
                        view,
                        op,
                        sender: self.me,
                    },
                );

                let oks = self.collect_prepare_ok(&self.rounds.current(), 0, self.nodes.len());
                if oks.len() >= quorum {
                    self.mark_committed(op);
                    self.rounds.advance(Key::Op);
                } else {
                    // Failed to commit -> trigger a view change (advance view) and stop normal ops.
                    stable = false;
                    self.rounds.advance(Key::View);
                    break;
                }
            } else {
                // Backup: receive Prepare (allow op >= current to catch up).
                let prepares = self.collect_prepare(&r, 0, self.nodes.len());
                let chosen = prepares
                    .into_iter()
                    .find(|(m, _)| matches!(m, Message::Prepare { sender, .. } if *sender == primary));

                let Some((msg, stamp)) = chosen else {
                    stable = false;
                    self.rounds.advance(Key::View);
                    break;
                };

                let (op, value, commit_hint) = match msg {
                    Message::Prepare { op, value, commit_hint, .. } => (op, value, commit_hint),
                    _ => panic!("expected Prepare"),
                };

                self.rounds.jump(&stamp);
                self.ensure_op_value(op, value);
                if let Some((committed_op, committed_value)) = commit_hint {
                    self.ensure_op_value(committed_op, committed_value);
                    self.mark_committed(committed_op);
                }

                // Send PrepareOk.
                let _ok_round = self.rounds.advance(Key::OpRound);
                comm_close::send(
                    primary,
                    Message::PrepareOk {
                        view,
                        op,
                        sender: self.me,
                    },
                );

                // Next op.
                self.rounds.advance(Key::Op);
            }
        }
        stable
    }

    fn next_view_round(&mut self) -> comm_close::Round {
        if !self.started {
            self.started = true;
        }
        // The RoundScheme resets (Kind/VcRound/Op/OpRound) automatically when we advance `View`.
        self.rounds.current()
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            comm_close::send(*node, msg.clone());
        }
    }

    fn ensure_op_value(&mut self, op: u32, value: u32) {
        let idx = op as usize;
        while self.log.len() <= idx {
            self.log.push(LogEntry { value: 0, committed: false });
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
        for (i, e) in self.log.iter().enumerate().rev() {
            if e.committed {
                return Some((i as u32, e.value));
            }
        }
        None
    }

    fn better_log(candidate: &[LogEntry], current: &[LogEntry]) -> bool {
        let cand_committed = candidate.iter().filter(|e| e.committed).count();
        let curr_committed = current.iter().filter(|e| e.committed).count();
        if cand_committed != curr_committed {
            return cand_committed > curr_committed;
        }
        candidate.len() > current.len()
    }

    fn collect_do_view_change(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::View, DefaultMatch::Eq)
            .level_cmp(Key::Kind, DefaultMatch::Eq)
            .level_cmp(Key::VcRound, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .filter(|(m, _)| matches!(m, Message::DoViewChange { .. }))
            .collect()
    }

    fn collect_start_view(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::View, DefaultMatch::Eq)
            .level_cmp(Key::Kind, DefaultMatch::Eq)
            .level_cmp(Key::VcRound, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .filter(|(m, _)| matches!(m, Message::StartView { .. }))
            .collect()
    }

    fn collect_prepare(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::View, DefaultMatch::Eq)
            .level_cmp(Key::Kind, DefaultMatch::Eq)
            .level_cmp(Key::Op, DefaultMatch::Gte)
            .level_cmp(Key::OpRound, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .filter(|(m, _)| matches!(m, Message::Prepare { .. }))
            .collect()
    }

    fn collect_prepare_ok(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<Message> {
        let filter = round
            .filter()
            .level_cmp(Key::View, DefaultMatch::Eq)
            .level_cmp(Key::Kind, DefaultMatch::Eq)
            .level_cmp(Key::Op, DefaultMatch::Eq)
            .level_cmp(Key::OpRound, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .map(|(m, _)| m)
            .filter(|m| matches!(m, Message::PrepareOk { .. }))
            .collect()
    }

    fn collect_messages_with_filter(
        &self,
        round: &comm_close::Round,
        filter: &comm_close::RoundFilter,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        assert!(max >= min, "requires max >= min");
        match self.mode {
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

fn start_node(scheme: RoundScheme, max_views: u32, max_ops: u32, mode: ReceiveMode) -> Vec<LogEntry> {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, scheme, max_views, max_ops, mode).run()
}

fn assert_committed_consistency(logs: &[Vec<LogEntry>]) {
    let max_len = logs.iter().map(|l| l.len()).max().unwrap_or(0);
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

fn run_protocol(num_nodes: usize, max_views: u32, max_ops: u32, mode: ReceiveMode) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let scheme = RoundScheme::builder()
            .from_u32(Key::View)
            .from_enum::<Kind>(Key::Kind)
            .from_enum::<VcRound>(Key::VcRound)
            .from_u32(Key::Op)
            .from_enum::<OpRound>(Key::OpRound)
            .build();

        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            let scheme = scheme.clone();
            let mode = mode;
            handles.push(thread::spawn(move || start_node(scheme, max_views, max_ops, mode)));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(nodes.clone()));
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }

        assert_committed_consistency(&logs);
    })
}

fn parse_args() -> (usize, u32, u32, ReceiveMode) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut views = DEFAULT_MAX_VIEWS;
    let mut ops = DEFAULT_MAX_OPS;
    let mut mode: Option<ReceiveMode> = None;

    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        if arg == "recv" {
            mode = Some(ReceiveMode::Recv);
        } else if arg == "inbox" {
            mode = Some(ReceiveMode::Inbox);
        } else if arg == "--nodes" {
            let value = args.next().unwrap_or_else(|| panic!("--nodes requires a value"));
            num_nodes = value.parse().unwrap_or_else(|_| panic!("invalid --nodes value: {}", value));
        } else if arg == "--views" {
            let value = args.next().unwrap_or_else(|| panic!("--views requires a value"));
            views = value.parse().unwrap_or_else(|_| panic!("invalid --views value: {}", value));
        } else if arg == "--ops" {
            let value = args.next().unwrap_or_else(|| panic!("--ops requires a value"));
            ops = value.parse().unwrap_or_else(|_| panic!("invalid --ops value: {}", value));
        } else {
            panic!("unknown argument: {}", arg);
        }
    }

    let mode = mode.unwrap_or_else(|| panic!("Must specify recv or inbox!"));
    (num_nodes, views, ops, mode)
}

fn main() {
    let (num_nodes, views, ops, mode) = parse_args();
    let stats = run_protocol(num_nodes, views, ops, mode);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
