use std::collections::BTreeMap;
use std::sync::Arc;

use traceforge::comm_close::{self, RoundScheme, RoundStamp, Rounds, TagCmp};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

// Keep bounds explicit and small for verification.
const NUM_NODES: usize = 4;
const NUM_ROUNDS: u32 = 3;

// C code waits for n/2 replies; primary counts itself implicitly.
const QUORUM: usize = NUM_NODES / 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Participants {
    nodes: [ThreadId; NUM_NODES],
}

impl Participants {
    fn from_vec(nodes: Vec<ThreadId>) -> Self {
        let nodes: [ThreadId; NUM_NODES] = nodes
            .try_into()
            .unwrap_or_else(|_| panic!("expected {} participants", NUM_NODES));
        Self { nodes }
    }

    fn len(&self) -> usize {
        self.nodes.len()
    }

    fn iter(&self) -> std::slice::Iter<'_, ThreadId> {
        self.nodes.iter()
    }

    fn get(&self, idx: usize) -> ThreadId {
        self.nodes[idx]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    DoViewChange,
    StartView,
    Prepare,
    PrepareOk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogEntry {
    view: u32,
    op_number: u32,
    committed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DoViewChangeMsg {
    stamp: RoundStamp,
    replica: ThreadId,
    log: Vec<LogEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StartViewMsg {
    stamp: RoundStamp,
    primary: ThreadId,
    log: Vec<LogEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrepareMsg {
    stamp: RoundStamp,
    view: u32,
    op_number: u32,
    has_request: bool,
    commit_hint: Option<u32>,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrepareOkMsg {
    stamp: RoundStamp,
    view: u32,
    op_number: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    DoViewChange(DoViewChangeMsg),
    StartView(StartViewMsg),
    Prepare(PrepareMsg),
    PrepareOk(PrepareOkMsg),
}

type RoundCollector = dyn Fn(
        &comm_close::Round,
        &comm_close::RoundFilter,
        usize,
        Option<usize>,
    ) -> Vec<Message>
    + Send
    + Sync;

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds,
    log: Vec<LogEntry>,
    op_number: u32,
    started: bool,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
        Self {
            nodes,
            me,
            rounds,
            log: Vec::new(),
            op_number: 0,
            started: false,
        }
    }

    fn run(mut self, collect: &RoundCollector) -> Vec<LogEntry> {
        for _ in 0..NUM_ROUNDS {
            self.step_view(collect);
        }
        self.log
    }

    fn step_view(&mut self, collect: &RoundCollector) {
        let dvc_round = self.next_view();
        let view = self.view_of(&dvc_round.stamp());
        let primary = self.primary_for_view(view);

        if self.me != primary {
            let msg = DoViewChangeMsg {
                stamp: dvc_round.stamp(),
                replica: self.me,
                log: self.log.clone(),
            };
            comm_close::send(primary, Message::DoViewChange(msg), &dvc_round);
        }

        if self.me == primary {
            let mut logs = vec![self.log.clone()];
            let recvd = self.collect_do_view_change(&dvc_round, collect, 0, Some(NUM_NODES * 2));
            for msg in recvd {
                logs.push(msg.log);
            }
            if logs.len().saturating_sub(1) < QUORUM {
                self.rounds.advance_round();
                return;
            }
            self.log = Self::choose_log(&logs);
        }

        let sv_round = self.rounds.advance_level(1);
        if self.me == primary {
            let msg = StartViewMsg {
                stamp: sv_round.stamp(),
                primary: self.me,
                log: self.log.clone(),
            };
            self.broadcast(&sv_round, Message::StartView(msg));
        } else {
            let mut msgs = self.collect_start_view(&sv_round, collect, 0, Some(1));
            if let Some(msg) = msgs.pop() {
                self.log = msg.log;
                self.sync_op_number();
            } else {
                self.rounds.advance_round();
                return;
            }
        }

        let prepare_round = self.rounds.advance_level(1);
        let mut prepared_op: Option<u32> = None;
        if self.me == primary {
            let has_request = traceforge::nondet();
            let op_number = self.op_number;
            let msg = PrepareMsg {
                stamp: prepare_round.stamp(),
                view,
                op_number,
                has_request,
                commit_hint: self.last_committed_op(),
                sender: self.me,
            };
            if has_request {
                self.log.push(LogEntry {
                    view,
                    op_number,
                    committed: false,
                });
                self.op_number += 1;
                prepared_op = Some(op_number);
            }
            self.broadcast(&prepare_round, Message::Prepare(msg));
        } else {
            let mut msgs = self.collect_prepare(&prepare_round, collect, 0, Some(1));
            let msg = match msgs.pop() {
                Some(msg) => msg,
                None => {
                    self.rounds.advance_round();
                    return;
                }
            };
            if let Some(commit) = msg.commit_hint {
                self.mark_committed(commit);
            }
            if msg.has_request && msg.op_number == self.op_number {
                self.log.push(LogEntry {
                    view: msg.view,
                    op_number: msg.op_number,
                    committed: false,
                });
                self.op_number += 1;
                prepared_op = Some(msg.op_number);
            }
        }

        let prepare_ok_round = self.rounds.advance_level(1);
        if self.me == primary {
            if let Some(op_number) = prepared_op {
                let recvd =
                    self.collect_prepare_ok(&prepare_ok_round, collect, 0, Some(NUM_NODES * 2));
                let ok_count = recvd
                    .iter()
                    .filter(|msg| msg.op_number == op_number)
                    .count();
                if ok_count >= QUORUM {
                    self.mark_committed(op_number);
                }
            }
        } else if let Some(op_number) = prepared_op {
            let msg = PrepareOkMsg {
                stamp: prepare_ok_round.stamp(),
                view,
                op_number,
                sender: self.me,
            };
            comm_close::send(primary, Message::PrepareOk(msg), &prepare_ok_round);
        }

        self.rounds.advance_round();
    }

    fn next_view(&mut self) -> comm_close::Round {
        if self.started {
            self.rounds.advance_round()
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn view_of(&self, stamp: &RoundStamp) -> u32 {
        stamp.components()[0]
    }

    fn primary_for_view(&self, view: u32) -> ThreadId {
        let idx = (view as usize) % self.nodes.len();
        self.nodes.get(idx)
    }

    fn broadcast(&self, round: &comm_close::Round, msg: Message) {
        for node in self.nodes.iter() {
            if *node != self.me {
                comm_close::send(*node, msg.clone(), round);
            }
        }
    }

    fn sync_op_number(&mut self) {
        let next = self
            .log
            .iter()
            .map(|e| e.op_number)
            .max()
            .map(|v| v + 1)
            .unwrap_or(0);
        self.op_number = next;
    }

    fn last_committed_op(&self) -> Option<u32> {
        self.log
            .iter()
            .filter(|e| e.committed)
            .map(|e| e.op_number)
            .max()
    }

    fn mark_committed(&mut self, op_number: u32) {
        if let Some(entry) = self.log.iter_mut().find(|e| e.op_number == op_number) {
            entry.committed = true;
        }
    }

    fn choose_log(logs: &[Vec<LogEntry>]) -> Vec<LogEntry> {
        let mut best = logs[0].clone();
        let mut best_committed = Self::committed_count(&best);
        for log in logs.iter().skip(1) {
            let count = Self::committed_count(log);
            if count > best_committed {
                best = log.clone();
                best_committed = count;
            }
        }
        best
    }

    fn committed_count(log: &[LogEntry]) -> usize {
        log.iter().filter(|e| e.committed).count()
    }

    fn collect_do_view_change(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<DoViewChangeMsg> {
        let filter = round.filter();
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::DoViewChange(payload) => Some(payload),
                _ => panic!("expected DoViewChangeMsg"),
            })
            .collect()
    }

    fn collect_start_view(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<StartViewMsg> {
        let filter = round.filter();
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::StartView(payload) => Some(payload),
                _ => panic!("expected StartViewMsg"),
            })
            .collect()
    }

    fn collect_prepare(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<PrepareMsg> {
        let filter = round.filter();
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Prepare(payload) => Some(payload),
                _ => panic!("expected PrepareMsg"),
            })
            .collect()
    }

    fn collect_prepare_ok(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<PrepareOkMsg> {
        let filter = round.filter();
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::PrepareOk(payload) => Some(payload),
                _ => panic!("expected PrepareOkMsg"),
            })
            .collect()
    }
}

fn start_node(collect: &RoundCollector, scheme: RoundScheme) -> Vec<LogEntry> {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, scheme).run(collect)
}

fn assert_consistent_commits(logs: &[Vec<LogEntry>]) {
    let mut by_view: BTreeMap<u32, Option<u32>> = BTreeMap::new();
    for log in logs {
        for entry in log.iter().filter(|e| e.committed) {
            let slot = by_view.entry(entry.view).or_insert(None);
            if let Some(prev) = *slot {
                assert_eq!(prev, entry.op_number);
            } else {
                *slot = Some(entry.op_number);
            }
        }
    }
}

fn run_protocol(collect: Arc<RoundCollector>) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        // Tag layout (outer -> inner): (view, phase).
        // - view: use Eq to keep messages scoped to the current view.
        // - phase: steps within a view.
        let scheme = RoundScheme::builder()
            .level("view", TagCmp::Eq)
            .level("phase", TagCmp::Eq)
            .build();

        let mut handles = Vec::new();
        for _ in 0..NUM_NODES {
            let receive = collect.clone();
            let scheme = scheme.clone();
            handles.push(thread::spawn(move || start_node(receive.as_ref(), scheme)));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(nodes));
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }
        assert_consistent_commits(&logs);
    })
}

fn run_protocol_with_recv() -> traceforge::Stats {
    let collect: Arc<RoundCollector> = Arc::new(|round, filter, min, max| {
        let upper = match max {
            Some(upper) => upper,
            None => (NUM_ROUNDS as usize) * 8 * NUM_NODES,
        };
        assert!(upper >= min, "requires max >= min");
        let count = if upper == min {
            min
        } else {
            (min..=upper).nondet()
        };

        let mut out = Vec::new();
        for _ in 0..count {
            let msg = comm_close::recv_block_with_filter::<Message>(filter);
            out.push(msg.payload(round).clone());
        }
        out
    });
    run_protocol(collect)
}

fn run_protocol_with_inbox() -> traceforge::Stats {
    let collect: Arc<RoundCollector> = Arc::new(|round, filter, min, max| {
        comm_close::inbox_with_bounds_filter(filter, min, max)
            .into_iter()
            .flatten()
            .filter_map(|msg| {
                msg.payload(round)
                    .as_any_ref()
                    .downcast_ref::<Message>()
                    .cloned()
            })
            .collect()
    });
    run_protocol(collect)
}

fn main() {
    let use_recv = std::env::args().any(|arg| arg == "recv");
    let use_inbox = std::env::args().any(|arg| arg == "inbox");

    if use_recv && use_inbox {
        panic!("Can't use recv/inbox at the same time!");
    } else if !use_recv && !use_inbox {
        panic!("Must specify recv or inbox!");
    }

    let stats = if use_recv {
        run_protocol_with_recv()
    } else {
        run_protocol_with_inbox()
    };
    println!("Stats = {}, {}", stats.execs, stats.block);
}
