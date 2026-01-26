use std::collections::BTreeMap;
use std::sync::Arc;

use traceforge::comm_close::{self, RoundScheme, RoundStamp, Rounds, TagCmp};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

// Keep bounds explicit and small for verification.
const NUM_NODES: usize = 3;
const NUM_ROUNDS: u32 = 2;

// Strict majority (> n/2) like the C code.
const QUORUM: usize = NUM_NODES / 2;

fn max_log_len() -> usize {
    (NUM_ROUNDS as usize).saturating_mul(NUM_NODES)
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogEntry {
    op: i32,
    committed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NewEpochMsg {
    stamp: RoundStamp,
    epoch: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AckEpochMsg {
    stamp: RoundStamp,
    epoch: u32,
    log: Vec<LogEntry>,
    history_len: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NewLeaderMsg {
    stamp: RoundStamp,
    epoch: u32,
    leader: ThreadId,
    log: Vec<LogEntry>,
    history_len: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BcastFirstMsg {
    stamp: RoundStamp,
    epoch: u32,
    slot: u32,
    op: i32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BcastSecondMsg {
    stamp: RoundStamp,
    epoch: u32,
    slot: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BcastThirdMsg {
    stamp: RoundStamp,
    epoch: u32,
    slot: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    NewEpoch(NewEpochMsg),
    AckEpoch(AckEpochMsg),
    NewLeader(NewLeaderMsg),
    BcastFirst(BcastFirstMsg),
    BcastSecond(BcastSecondMsg),
    BcastThird(BcastThirdMsg),
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
        }
    }

    fn run(mut self, collect: &RoundCollector) -> Vec<LogEntry> {
        for _ in 0..NUM_ROUNDS {
            self.step_epoch(collect);
        }
        self.log
    }

    fn step_epoch(&mut self, collect: &RoundCollector) {
        let new_epoch_round = self.rounds.current();
        if self.coord() {
            self.run_leader(&new_epoch_round, collect);
        } else {
            self.run_follower(&new_epoch_round, collect);
        }
    }

    fn run_leader(&mut self, new_epoch_round: &comm_close::Round, collect: &RoundCollector) {
        let epoch = new_epoch_round.level(0);
        let msg = NewEpochMsg {
            stamp: new_epoch_round.stamp(),
            epoch,
            sender: self.me,
        };
        self.broadcast(new_epoch_round, Message::NewEpoch(msg));

        let ack_round = self.rounds.advance_level(1);
        let acks = self.collect_ack_epoch(&ack_round, collect, 0, Some(self.nodes.len() - 1));
        if acks.len() <= QUORUM {
            self.rounds.advance_round();
            return;
        }

        self.log = Self::choose_longest_log(&acks);

        let nl_round = self.rounds.advance_level(1);
        let msg = NewLeaderMsg {
            stamp: nl_round.stamp(),
            epoch,
            leader: self.me,
            log: self.log.clone(),
            history_len: Self::history_len(&self.log),
        };
        self.broadcast(&nl_round, Message::NewLeader(msg));

        self.rounds.advance_level(1);
        self.run_bcast_leader(collect);
        self.rounds.advance_round();
    }

    fn run_follower(&mut self, new_epoch_round: &comm_close::Round, collect: &RoundCollector) {
        let mut msgs = self.collect_new_epoch(new_epoch_round, collect, 0, Some(1));
        let msg = match msgs.pop() {
            Some(msg) => msg,
            None => {
                self.rounds.advance_round();
                return;
            }
        };

        if msg.stamp.gt_at(&new_epoch_round.stamp(), 0) {
            self.rounds.jump(&msg.stamp);
        }

        let epoch = msg.epoch;
        let leader = msg.sender;

        let ack_round = self.rounds.advance_level(1);
        let ack = AckEpochMsg {
            stamp: ack_round.stamp(),
            epoch,
            log: self.log.clone(),
            history_len: Self::history_len(&self.log),
            sender: self.me,
        };
        comm_close::send(leader, Message::AckEpoch(ack), &ack_round);

        let nl_round = self.rounds.advance_level(1);
        let mut nl_msgs = self.collect_new_leader(&nl_round, collect, 0, Some(1));
        let nl_msg = match nl_msgs.pop() {
            Some(msg) => msg,
            None => {
                self.rounds.advance_round();
                return;
            }
        };
        if nl_msg.leader != leader {
            self.rounds.advance_round();
            return;
        }
        self.log = nl_msg.log;
        self.truncate_log();

        self.rounds.advance_level(1);
        self.run_bcast_follower(collect, leader);
        self.rounds.advance_round();
    }

    fn run_bcast_leader(&mut self, collect: &RoundCollector) {
        self.ensure_uncommitted_entry(true);
        let target_slot = self.last_index();
        self.sync_slot(target_slot);

        for _ in 0..max_log_len() {
            let slot = self.rounds.current().level(2) as usize;
            if slot >= max_log_len() {
                break;
            }

            let first_round = self.rounds.current();
            let epoch = first_round.level(0);
            let op = self.log[slot].op;
            let msg = BcastFirstMsg {
                stamp: first_round.stamp(),
                epoch,
                slot: slot as u32,
                op,
                sender: self.me,
            };
            self.broadcast(&first_round, Message::BcastFirst(msg));

            let second_round = self.rounds.advance_level(3);
            let mut acks = vec![BcastSecondMsg {
                stamp: second_round.stamp(),
                epoch,
                slot: slot as u32,
                sender: self.me,
            }];
            let mut recvd = self.collect_bcast_second(&second_round, collect, 0, Some(self.nodes.len() - 1));
            acks.append(&mut recvd);
            if acks.len() <= QUORUM {
                break;
            }

            self.commit_slot(slot);
            let third_round = self.rounds.advance_level(3);
            let msg = BcastThirdMsg {
                stamp: third_round.stamp(),
                epoch,
                slot: slot as u32,
                sender: self.me,
            };
            self.broadcast(&third_round, Message::BcastThird(msg));

            if !self.append_entry(true) {
                break;
            }
            self.rounds.advance_level(2);
        }
    }

    fn run_bcast_follower(&mut self, collect: &RoundCollector, leader: ThreadId) {
        self.ensure_uncommitted_entry(false);
        let target_slot = self.last_index();
        self.sync_slot(target_slot);

        for _ in 0..max_log_len() {
            let slot = self.rounds.current().level(2) as usize;
            if slot >= max_log_len() {
                break;
            }

            let first_round = self.rounds.current();
            let mut msgs = self.collect_bcast_first(&first_round, collect, 0, Some(1));
            let msg = match msgs.pop() {
                Some(msg) => msg,
                None => break,
            };
            if msg.sender != leader {
                break;
            }

            self.ensure_slot(slot);
            if let Some(entry) = self.log.get_mut(slot) {
                entry.op = msg.op;
                entry.committed = false;
            }

            let second_round = self.rounds.advance_level(3);
            let ack = BcastSecondMsg {
                stamp: second_round.stamp(),
                epoch: second_round.level(0),
                slot: slot as u32,
                sender: self.me,
            };
            comm_close::send(leader, Message::BcastSecond(ack), &second_round);

            let third_round = self.rounds.advance_level(3);
            let mut commits = self.collect_bcast_third(&third_round, collect, 0, Some(1));
            let msg = match commits.pop() {
                Some(msg) => msg,
                None => break,
            };
            if msg.sender != leader {
                break;
            }

            self.commit_slot(slot);
            if !self.append_entry(false) {
                break;
            }
            self.rounds.advance_level(2);
        }
    }

    fn coord(&self) -> bool {
        traceforge::nondet()
    }

    fn broadcast(&self, round: &comm_close::Round, msg: Message) {
        for node in self.nodes.iter() {
            if *node != self.me {
                comm_close::send(*node, msg.clone(), round);
            }
        }
    }

    fn last_index(&self) -> u32 {
        if self.log.is_empty() {
            0
        } else {
            (self.log.len() - 1) as u32
        }
    }

    fn ensure_uncommitted_entry(&mut self, leader: bool) {
        let needs_entry = self.log.is_empty() || self.log.last().map_or(false, |e| e.committed);
        if needs_entry {
            let _ = self.append_entry(leader);
        }
    }

    fn ensure_slot(&mut self, slot: usize) {
        while self.log.len() <= slot && self.log.len() < max_log_len() {
            self.log.push(LogEntry {
                op: -1,
                committed: false,
            });
        }
    }

    fn append_entry(&mut self, leader: bool) -> bool {
        if self.log.len() >= max_log_len() {
            return false;
        }
        let op = if leader { Self::input_op() } else { -1 };
        self.log.push(LogEntry {
            op,
            committed: false,
        });
        true
    }

    fn commit_slot(&mut self, slot: usize) {
        if let Some(entry) = self.log.get_mut(slot) {
            entry.committed = true;
        }
    }

    fn sync_slot(&mut self, target: u32) {
        let mut current = self.rounds.current().level(2);
        while current < target {
            let round = self.rounds.advance_level(2);
            current = round.level(2);
        }
    }

    fn truncate_log(&mut self) {
        if self.log.len() > max_log_len() {
            self.log.truncate(max_log_len());
        }
    }

    fn history_len(log: &[LogEntry]) -> u32 {
        log.len().saturating_sub(1) as u32
    }

    fn choose_longest_log(acks: &[AckEpochMsg]) -> Vec<LogEntry> {
        let mut best = Vec::new();
        let mut best_len: Option<u32> = None;
        for ack in acks {
            let len = ack.history_len;
            if best_len.map_or(true, |cur| len >= cur) {
                best_len = Some(len);
                best = ack.log.clone();
            }
        }
        if best.len() > max_log_len() {
            best.truncate(max_log_len());
        }
        best
    }

    fn input_op() -> i32 {
        if traceforge::nondet() {
            1
        } else {
            2
        }
    }

    fn collect_new_epoch(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<NewEpochMsg> {
        let filter = round.filter();
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::NewEpoch(payload) => Some(payload),
                _ => panic!("expected NewEpochMsg"),
            })
            .collect()
    }

    fn collect_ack_epoch(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<AckEpochMsg> {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::AckEpoch(payload) => Some(payload),
                _ => panic!("expected AckEpochMsg"),
            })
            .collect()
    }

    fn collect_new_leader(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<NewLeaderMsg> {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::NewLeader(payload) => Some(payload),
                _ => panic!("expected NewLeaderMsg"),
            })
            .collect()
    }

    fn collect_bcast_first(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<BcastFirstMsg> {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::BcastFirst(payload) => Some(payload),
                _ => panic!("expected BcastFirstMsg"),
            })
            .collect()
    }

    fn collect_bcast_second(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<BcastSecondMsg> {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::BcastSecond(payload) => Some(payload),
                _ => panic!("expected BcastSecondMsg"),
            })
            .collect()
    }

    fn collect_bcast_third(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<BcastThirdMsg> {
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::BcastThird(payload) => Some(payload),
                _ => panic!("expected BcastThirdMsg"),
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
    let mut per_slot: BTreeMap<usize, Option<i32>> = BTreeMap::new();
    for log in logs {
        for (slot, entry) in log.iter().enumerate() {
            if !entry.committed {
                continue;
            }
            let chosen = per_slot.entry(slot).or_insert(None);
            if let Some(prev) = *chosen {
                assert_eq!(prev, entry.op);
            } else {
                *chosen = Some(entry.op);
            }
        }
    }
}

fn run_protocol(collect: Arc<RoundCollector>) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        // Tag layout (outer -> inner): (epoch, phase, slot, bphase).
        // - epoch: allow >= to model jumps to newer epochs.
        // - phase/slot/bphase: exact match for each step.
        let scheme = RoundScheme::builder()
            .level("epoch", TagCmp::Gte)
            .level("phase", TagCmp::Eq)
            .level("slot", TagCmp::Eq)
            .level("bphase", TagCmp::Eq)
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
            None => (NUM_ROUNDS as usize) * max_log_len() * 8 * NUM_NODES,
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
