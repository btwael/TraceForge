use std::collections::BTreeMap;
use std::sync::Arc;

use traceforge::comm_close::{self, RoundScheme, RoundStamp, Rounds, TagCmp};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

// Keep bounds explicit and small for verification.
const NUM_NODES: usize = 3;
const NUM_ROUNDS: u32 = 1;

// Majority quorum for view change.
const QUORUM: usize = (NUM_NODES / 2) + 1;

// Keep logs tiny.
const MAX_LOG_LEN: usize = 3;

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

    fn index_of(&self, id: ThreadId) -> usize {
        self.nodes
            .iter()
            .position(|x| *x == id)
            .expect("participant not found")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Aux,
    StartViewChange,
    DoViewChange,
    StartView,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StartViewChangeMsg {
    stamp: RoundStamp,
    replica: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DoViewChangeMsg {
    stamp: RoundStamp,
    replica: ThreadId,
    prev_view: u32,
    log: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StartViewMsg {
    stamp: RoundStamp,
    primary: ThreadId,
    log: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    StartViewChange(StartViewChangeMsg),
    DoViewChange(DoViewChangeMsg),
    StartView(StartViewMsg),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogEntry {
    view: u32,
    log: Vec<bool>,
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

    // View-change state.
    prev_view: u32,
    log: Vec<bool>,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme) -> Self {
        let me = thread::current().id();
        let mut rounds = Rounds::with_scheme(scheme);

        // Like the C version, start in a random view within [0, n-1].
        let start_view: usize = (0..=NUM_NODES - 1).nondet();
        for _ in 0..start_view {
            rounds.advance_round();
        }

        let log = Self::init_log();

        Self {
            nodes,
            me,
            rounds,
            prev_view: start_view as u32,
            log,
        }
    }

    fn run(mut self, collect: &RoundCollector) -> Vec<LogEntry> {
        let mut out = Vec::new();
        for _ in 0..NUM_ROUNDS {
            self.step_view_change(collect, &mut out);
        }
        out
    }

    fn step_view_change(&mut self, collect: &RoundCollector, out: &mut Vec<LogEntry>) {
        let _aux = self.rounds.current();
        let svc_round = self.rounds.advance_level(1);
        let view = self.view_of(&svc_round.stamp());

        let primary = self.primary_for_view(view);

        // --- Send StartViewChange to everyone ---
        let svc_msg = StartViewChangeMsg {
            stamp: svc_round.stamp(),
            replica: self.me,
        };
        self.broadcast(&svc_round, Message::StartViewChange(svc_msg));

        // Collect a bounded, nondet number of messages from this view or higher views
        // (and from StartViewChange/DoViewChange/StartView phases).
        let mut mbox = self.collect_any_vc_msgs(&svc_round, collect, 0, Some(NUM_NODES * 4));

        // --- Jump rule: if we see a higher view, jump to it ---
        if let Some(target) = self.max_view_in_mbox(&mbox).filter(|v| *v > view) {
            self.prev_view = view;
            self.jump_to_view(target);
            return;
        }

        // --- StartViewChange quorum condition ---
        let svc_count = 1 + Self::count_svc_for_view(&mbox, view);
        let saw_dvc_for_view = Self::has_dvc_for_view(&mbox, view);

        if svc_count < QUORUM && !saw_dvc_for_view {
            // Timeout / failed attempt: go to next view.
            self.prev_view = view;
            self.rounds.advance_round();
            return;
        }

        // --- DoViewChange phase ---
        let dvc_round = self.rounds.advance_level(1);

        if self.me != primary {
            // Replica sends DoViewChange to primary.
            let dvc_msg = DoViewChangeMsg {
                stamp: dvc_round.stamp(),
                replica: self.me,
                prev_view: self.prev_view,
                log: self.log.clone(),
            };
            comm_close::send(primary, Message::DoViewChange(dvc_msg), &dvc_round);
        }

        // Primary tries to gather a quorum of DoViewChange messages and then sends StartView.
        let mut chosen_log: Option<Vec<bool>> = None;

        if self.me == primary {
            // Include our own DoViewChange as an implicit message.
            let mut dvc_msgs = vec![DoViewChangeMsg {
                stamp: dvc_round.stamp(),
                replica: self.me,
                prev_view: self.prev_view,
                log: self.log.clone(),
            }];

            // Pull more messages (still allowing higher-view jumps).
            let more = self.collect_any_vc_msgs(&dvc_round, collect, 0, Some(NUM_NODES * 4));
            mbox.extend(more);

            if let Some(target) = self.max_view_in_mbox(&mbox).filter(|v| *v > view) {
                self.prev_view = view;
                self.jump_to_view(target);
                return;
            }

            // Take all DoViewChange for this view.
            for msg in &mbox {
                if let Message::DoViewChange(m) = msg {
                    if self.view_of(&m.stamp) == view {
                        dvc_msgs.push(m.clone());
                    }
                }
            }

            if dvc_msgs.len() >= QUORUM {
                chosen_log = Some(Self::choose_log(&dvc_msgs));
                self.log = chosen_log.clone().unwrap();
            } else {
                // Failed attempt: go to next view.
                self.prev_view = view;
                self.rounds.advance_round();
                return;
            }
        }

        // --- StartView phase ---
        let sv_round = self.rounds.advance_level(1);

        if self.me == primary {
            // Primary broadcasts StartView with selected log.
            let msg = StartViewMsg {
                stamp: sv_round.stamp(),
                primary: self.me,
                log: chosen_log.unwrap_or_else(|| self.log.clone()),
            };
            self.broadcast(&sv_round, Message::StartView(msg));

            // "Start NormalOp": record that we installed a view/log.
            out.push(LogEntry {
                view,
                log: self.log.clone(),
            });
        } else {
            // Replica waits (nondeterministically) for StartView, but can still jump on a higher view.
            let msgs = self.collect_any_vc_msgs(&sv_round, collect, 0, Some(NUM_NODES * 4));

            if let Some(target) = self.max_view_in_mbox(&msgs).filter(|v| *v > view) {
                self.prev_view = view;
                self.jump_to_view(target);
                return;
            }

            if let Some(start_view) = msgs.iter().find_map(|m| match m {
                Message::StartView(sv) if self.view_of(&sv.stamp) == view => Some(sv.clone()),
                _ => None,
            }) {
                self.log = start_view.log.clone();
                out.push(LogEntry {
                    view,
                    log: self.log.clone(),
                });
            }
        }

        // Move to next view (like the C code's view_nr++).
        self.prev_view = view;
        self.rounds.advance_round();
    }

    fn init_log() -> Vec<bool> {
        let len: usize = (0..=MAX_LOG_LEN).nondet();
        let mut log = Vec::new();
        for _ in 0..len {
            log.push(traceforge::nondet());
        }
        log
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

    fn jump_to_view(&mut self, target_view: u32) {
        // Advance the comm_close round counter until we reach target_view.
        let mut current = self.view_of(&self.rounds.current().stamp());
        while current < target_view {
            self.rounds.advance_round();
            current = self.view_of(&self.rounds.current().stamp());
        }
    }

    fn collect_any_vc_msgs(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<Message> {
        // We base the filter on StartViewChange, and relax the phase comparator to accept
        // StartViewChange/DoViewChange/StartView from this view or higher views.
        let filter = round.filter().level_cmp(1, TagCmp::Gte);
        collect(round, &filter, min, max)
    }

    fn max_view_in_mbox(&self, msgs: &[Message]) -> Option<u32> {
        let mut max: Option<u32> = None;
        for msg in msgs {
            let v = match msg {
                Message::StartViewChange(m) => self.view_of(&m.stamp),
                Message::DoViewChange(m) => self.view_of(&m.stamp),
                Message::StartView(m) => self.view_of(&m.stamp),
                Message::Init(_) => continue,
            };
            max = Some(max.map_or(v, |cur| cur.max(v)));
        }
        max
    }

    fn count_svc_for_view(msgs: &[Message], view: u32) -> usize {
        msgs.iter()
            .filter(|m| matches!(m, Message::StartViewChange(svc) if svc.stamp.components()[0] == view))
            .count()
    }

    fn has_dvc_for_view(msgs: &[Message], view: u32) -> bool {
        msgs.iter()
            .any(|m| matches!(m, Message::DoViewChange(dvc) if dvc.stamp.components()[0] == view))
    }

    fn choose_log(msgs: &[DoViewChangeMsg]) -> Vec<bool> {
        let mut best = msgs[0].log.clone();
        let mut best_committed = Self::committed_count(&best);

        for m in msgs.iter().skip(1) {
            let c = Self::committed_count(&m.log);
            if c > best_committed {
                best = m.log.clone();
                best_committed = c;
            }
        }
        best
    }

    fn committed_count(log: &[bool]) -> usize {
        log.iter().filter(|b| **b).count()
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

fn assert_consistent_start_view(logs: &[Vec<LogEntry>]) {
    // For each view: if multiple nodes install a log in that view, they must match.
    let mut seen: BTreeMap<u32, Vec<bool>> = BTreeMap::new();

    for log in logs {
        for entry in log {
            if let Some(prev) = seen.get(&entry.view) {
                assert_eq!(prev, &entry.log);
            } else {
                seen.insert(entry.view, entry.log.clone());
            }
        }
    }
}

fn run_protocol(collect: Arc<RoundCollector>) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        // Tag layout (outer -> inner): (view, phase).
        // - view: grows monotonically, so we use Gte by default (supports jumps to higher views).
        // - phase: allow higher phases by default to support jump-style receives.
        let scheme = RoundScheme::builder()
            .level("view", TagCmp::Gte)
            .level("phase", TagCmp::Gte)
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
        assert_consistent_start_view(&logs);
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
