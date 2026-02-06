use traceforge::comm_close::{self, DefaultMatch, RoundScheme, RoundStamp, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_SEQS: u32 = 1;
const DEFAULT_MAX_VIEWS_PER_SEQ: u32 = 1;
const MAX_VALUE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundKey)]
enum Key {
    Seq,
    View,
    Phase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum Phase {
    PrePrepare,
    Prepare,
    Commit,
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
struct PrePrepareMsg {
    seq: u32,
    view: u32,
    value: u32,
    primary: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrepareMsg {
    seq: u32,
    view: u32,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitMsg {
    seq: u32,
    view: u32,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    PrePrepare(PrePrepareMsg),
    Prepare(PrepareMsg),
    Commit(CommitMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogEntry {
    seq: u32,
    value: u32,
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds,
    started: bool,
    num_seqs: u32,
    max_views_per_seq: u32,
    mode: ReceiveMode,

    // "Current" value estimate for (seq, view). Cleared on view/seq advance.
    value: Option<u32>,
}

impl Node {
    fn new(
        nodes: Participants,
        scheme: RoundScheme,
        num_seqs: u32,
        max_views_per_seq: u32,
        mode: ReceiveMode,
    ) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
        Self {
            nodes,
            me,
            rounds,
            started: false,
            num_seqs,
            max_views_per_seq,
            mode,
            value: None,
        }
    }

    fn run(mut self) -> Vec<LogEntry> {
        let mut log = Vec::new();
        for _ in 0..self.num_seqs {
            self.step_seq(&mut log);
        }
        log
    }

    fn step_seq(&mut self, log: &mut Vec<LogEntry>) {
        // Start the next sequence number (outer level).
        let _seq_round = self.next_seq_round();
        self.value = None;

        // Try a bounded number of views for this seq. Timeout is modeled by receiving 0 messages.
        for _ in 0..self.max_views_per_seq {
            if self.try_decide_current_seq(log) {
                break;
            }
            // If not decided, we either timed out and advanced view, or jumped forward.
            // In all cases, we keep trying until we exhaust max_views_per_seq.
        }
    }

    fn next_seq_round(&mut self) -> comm_close::Round {
        if self.started {
            self.rounds.advance(Key::Seq)
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn try_decide_current_seq(&mut self, log: &mut Vec<LogEntry>) -> bool {
        let current = self.rounds.current();
        match current.get_enum::<_, Phase>(Key::Phase) {
            Phase::PrePrepare => {
                if !self.phase_pre_prepare(&current) {
                    return false;
                }
                let after = self.rounds.current();
                if after.get_enum::<_, Phase>(Key::Phase) == Phase::PrePrepare {
                    self.rounds.advance(Key::Phase);
                }
                self.try_decide_current_seq(log)
            }
            Phase::Prepare => {
                if !self.phase_prepare(&current) {
                    return false;
                }
                let after = self.rounds.current();
                if after.get_enum::<_, Phase>(Key::Phase) == Phase::Prepare {
                    self.rounds.advance(Key::Phase);
                }
                self.try_decide_current_seq(log)
            }
            Phase::Commit => self.phase_commit(&current, log),
        }
    }

    fn phase_pre_prepare(&mut self, round: &comm_close::Round) -> bool {
        let seq = round.get_u32(Key::Seq);
        let view = round.get_u32(Key::View);
        let primary = self.nodes.primary_for_view(view);

        // Deterministic "client value" for this seq: keeps the simplified model safe without full view-change.
        let client_value = Self::client_value(seq, self.me);

        // Primary proposes.
        if self.me == primary {
            self.value = Some(client_value);
            let msg = PrePrepareMsg {
                seq,
                view,
                value: client_value,
                primary,
                sender: self.me,
            };
            self.broadcast(Message::PrePrepare(msg));
        }

        // Everyone may receive one message from (seq, view, phase) or the future (catch-up).
        // If we are not the primary and receive nothing, we model a timeout by advancing view.
        let msgs = self.collect_messages(self.mode, round, 0, 1);
        if let Some((msg, stamp)) = msgs.first() {
            self.rounds.jump(stamp);
            match msg {
                Message::PrePrepare(payload) => self.value = Some(payload.value),
                Message::Prepare(payload) => self.value = Some(payload.value),
                Message::Commit(payload) => self.value = Some(payload.value),
                Message::Init(_) => panic!("unexpected Init"),
            }
            true
        } else if self.me != primary {
            // timeout
            self.rounds.advance(Key::View);
            self.value = None;
            false
        } else {
            // primary can continue without receiving
            true
        }
    }

    fn phase_prepare(&mut self, round: &comm_close::Round) -> bool {
        let seq = round.get_u32(Key::Seq);
        let view = round.get_u32(Key::View);

        let value = self.value.unwrap_or_else(|| Self::client_value(seq, self.me));
        self.value = Some(value);

        // Replica broadcasts Prepare.
        let msg = PrepareMsg {
            seq,
            view,
            value,
            sender: self.me,
        };
        self.broadcast(Message::Prepare(msg));

        // Collect some Prepare messages (eq on seq/view/phase). We model timeouts by receiving 0.
        let max = self.nodes.len();
        let prepares = self.collect_prepare(round, 0, max);

        let n = self.nodes.len();
        let f = (n.saturating_sub(1) / 3) as usize;
        let needed_from_others = 2 * f;

        if Self::count_matching_senders(&prepares, value, self.me) >= needed_from_others {
            true
        } else {
            // timeout / not prepared -> advance view (re-try with new primary)
            self.rounds.advance(Key::View);
            self.value = None;
            false
        }
    }

    fn phase_commit(&mut self, round: &comm_close::Round, log: &mut Vec<LogEntry>) -> bool {
        let seq = round.get_u32(Key::Seq);
        let view = round.get_u32(Key::View);

        let value = self.value.unwrap_or_else(|| Self::client_value(seq, self.me));
        self.value = Some(value);

        // Replica broadcasts Commit.
        let msg = CommitMsg {
            seq,
            view,
            value,
            sender: self.me,
        };
        self.broadcast(Message::Commit(msg));

        let max = self.nodes.len();
        let commits = self.collect_commit(round, 0, max);

        let n = self.nodes.len();
        let f = (n.saturating_sub(1) / 3) as usize;
        let needed_from_others = 2 * f;

        if Self::count_matching_senders(&commits, value, self.me) >= needed_from_others {
            self.record_decision(log, seq, value);
            true
        } else {
            // timeout / not committed -> advance view
            self.rounds.advance(Key::View);
            self.value = None;
            false
        }
    }

    fn record_decision(&self, log: &mut Vec<LogEntry>, seq: u32, value: u32) {
        if let Some(prev) = log.iter().find(|e| e.seq == seq) {
            assert_eq!(prev.value, value, "local decided two values for seq {}", seq);
            return;
        }
        log.push(LogEntry { seq, value });
    }

    fn client_value(seq: u32, client: ThreadId) -> u32 {
        (u32::from(client) * seq) % MAX_VALUE
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            comm_close::send(*node, msg.clone());
        }
    }

    fn collect_prepare(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(PrepareMsg, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Seq, DefaultMatch::Eq)
            .level_cmp(Key::View, DefaultMatch::Eq)
            .level_cmp(Key::Phase, DefaultMatch::Eq);
        self.collect_messages_with_filter(self.mode, round, &filter, min, max)
            .into_iter()
            .map(|(msg, stamp)| match msg {
                Message::Prepare(payload) => (payload, stamp),
                _ => panic!("expected Prepare"),
            })
            .collect()
    }

    fn collect_commit(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(CommitMsg, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Seq, DefaultMatch::Eq)
            .level_cmp(Key::View, DefaultMatch::Eq)
            .level_cmp(Key::Phase, DefaultMatch::Eq);
        self.collect_messages_with_filter(self.mode, round, &filter, min, max)
            .into_iter()
            .map(|(msg, stamp)| match msg {
                Message::Commit(payload) => (payload, stamp),
                _ => panic!("expected Commit"),
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

    fn count_matching_senders<M>(
        msgs: &[(M, RoundStamp)],
        value: u32,
        me: ThreadId,
    ) -> usize
    where
        M: MatchingValue + HasSender,
    {
        use std::collections::HashSet;
        let mut senders: HashSet<ThreadId> = HashSet::new();
        for (m, _) in msgs {
            if m.value() == value {
                senders.insert(m.sender());
            }
        }
        senders.remove(&me);
        senders.len()
    }
}

trait HasSender {
    fn sender(&self) -> ThreadId;
}

trait MatchingValue {
    fn value(&self) -> u32;
}

impl HasSender for PrepareMsg {
    fn sender(&self) -> ThreadId {
        self.sender
    }
}

impl MatchingValue for PrepareMsg {
    fn value(&self) -> u32 {
        self.value
    }
}

impl HasSender for CommitMsg {
    fn sender(&self) -> ThreadId {
        self.sender
    }
}

impl MatchingValue for CommitMsg {
    fn value(&self) -> u32 {
        self.value
    }
}

fn start_node(
    scheme: RoundScheme,
    num_seqs: u32,
    max_views_per_seq: u32,
    mode: ReceiveMode,
) -> Vec<LogEntry> {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, scheme, num_seqs, max_views_per_seq, mode).run()
}

fn assert_pbft_agreement(logs: &[Vec<LogEntry>]) {
    use std::collections::HashMap;
    let mut decided: HashMap<u32, u32> = HashMap::new();
    for log in logs {
        for entry in log {
            match decided.get(&entry.seq) {
                Some(prev) => assert_eq!(
                    *prev, entry.value,
                    "agreement violated at seq {}: {} vs {}",
                    entry.seq, prev, entry.value
                ),
                None => {
                    decided.insert(entry.seq, entry.value);
                }
            }
        }
    }
}

fn run_protocol(
    num_nodes: usize,
    num_seqs: u32,
    max_views_per_seq: u32,
    mode: ReceiveMode,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let scheme = RoundScheme::builder()
            // Outer: sequence number; inner: view changes; then PBFT phases.
            .from_u32(Key::Seq)
            .from_u32(Key::View)
            .from_enum::<Phase>(Key::Phase)
            .build();

        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            let scheme = scheme.clone();
            let num_seqs = num_seqs;
            let max_views_per_seq = max_views_per_seq;
            let mode = mode;
            handles.push(thread::spawn(move || {
                start_node(scheme, num_seqs, max_views_per_seq, mode)
            }));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(nodes.clone()));
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }
        assert_pbft_agreement(&logs);
    })
}

fn run_protocol_with_recv(num_nodes: usize, num_seqs: u32, max_views_per_seq: u32) -> traceforge::Stats {
    run_protocol(num_nodes, num_seqs, max_views_per_seq, ReceiveMode::Recv)
}

fn run_protocol_with_inbox(num_nodes: usize, num_seqs: u32, max_views_per_seq: u32) -> traceforge::Stats {
    run_protocol(num_nodes, num_seqs, max_views_per_seq, ReceiveMode::Inbox)
}

fn parse_args() -> (usize, u32, u32, ReceiveMode) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_seqs = DEFAULT_NUM_SEQS;
    let mut max_views_per_seq = DEFAULT_MAX_VIEWS_PER_SEQ;
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
        } else if arg == "--seqs" {
            let value = args.next().unwrap_or_else(|| panic!("--seqs requires a value"));
            num_seqs = value.parse().unwrap_or_else(|_| panic!("invalid --seqs value: {}", value));
        } else if arg == "--views" {
            let value = args.next().unwrap_or_else(|| panic!("--views requires a value"));
            max_views_per_seq = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid --views value: {}", value));
        } else {
            panic!("unknown argument: {}", arg);
        }
    }

    let mode = mode.unwrap_or_else(|| panic!("Must specify recv or inbox!"));
    (num_nodes, num_seqs, max_views_per_seq, mode)
}

fn main() {
    let (num_nodes, num_seqs, max_views_per_seq, mode) = parse_args();
    let stats = match mode {
        ReceiveMode::Recv => run_protocol_with_recv(num_nodes, num_seqs, max_views_per_seq),
        ReceiveMode::Inbox => run_protocol_with_inbox(num_nodes, num_seqs, max_views_per_seq),
    };
    println!("Stats = {}, {}", stats.execs, stats.block);
}
