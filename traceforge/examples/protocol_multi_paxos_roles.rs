
use traceforge::comm_close::{self, DefaultMatch, RoundScheme, RoundStamp, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_EPOCHS: u32 = 1;
const DEFAULT_SLOTS_PER_EPOCH: u32 = 1;
const MAX_VALUE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundKey)]
enum Key {
    Epoch,
    PhaseA,
    Slot,
    PhaseB,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum PhaseA {
    NewEpoch,
    AckEpoch,
    NewLeader,
    Bcast,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum PhaseB {
    Propose,
    Ack,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogEntry {
    value: u32,
    committed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NewEpochMsg {
    epoch: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AckEpochMsg {
    epoch: u32,
    sender: ThreadId,
    log: Vec<LogEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NewLeaderMsg {
    epoch: u32,
    sender: ThreadId,
    log: Vec<LogEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProposeMsg {
    epoch: u32,
    slot: u32,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AckMsg {
    epoch: u32,
    slot: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitMsg {
    epoch: u32,
    slot: u32,
    value: u32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),

    // Leader election / recovery.
    NewEpoch(NewEpochMsg),
    AckEpoch(AckEpochMsg),
    NewLeader(NewLeaderMsg),

    // Broadcast (Multi-Paxos) rounds.
    Propose(ProposeMsg),
    Ack(AckMsg),
    Commit(CommitMsg),
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds,
    mode: ReceiveMode,

    // Local replicated log.
    log: Vec<LogEntry>,

    max_epochs: u32,
    slots_per_epoch: u32,
    started: bool,
}

impl Node {
    fn new(
        nodes: Participants,
        scheme: RoundScheme,
        max_epochs: u32,
        slots_per_epoch: u32,
        mode: ReceiveMode,
    ) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
        Self {
            nodes,
            me,
            rounds,
            mode,
            log: Vec::new(),
            max_epochs,
            slots_per_epoch,
            started: false,
        }
    }

    fn run(mut self) -> Vec<LogEntry> {
        for _ in 0..self.max_epochs {
            if !self.run_one_epoch() {
                // If the epoch aborted, we just continue (epoch has already advanced).
                continue;
            }
        }
        self.log
    }

    fn coord(&self) -> bool {
        traceforge::nondet()
    }

    /// Runs one epoch. Returns true if we reached broadcast at least once in this epoch.
    fn run_one_epoch(&mut self) -> bool {
        let new_epoch_round = self.next_epoch_round();
        let epoch = new_epoch_round.get_u32(Key::Epoch);

        // Phase A1: NewEpoch broadcast by coordinator.
        if self.coord() {
            let msg = NewEpochMsg {
                epoch,
                sender: self.me,
            };
            self.broadcast(Message::NewEpoch(msg));
        }

        // Followers (and coordinator) wait for exactly one NewEpoch message (epoch >= current).
        let new_epochs = self.collect_new_epoch(&new_epoch_round, 0, self.nodes.len());
        if new_epochs.len() != 1 {
            // Timeout / ambiguity -> next epoch.
            self.rounds.advance(Key::Epoch);
            return false;
        }

        let (msg, stamp) = &new_epochs[0];
        self.rounds.jump(stamp);
        let epoch = match msg {
            Message::NewEpoch(payload) => payload.epoch,
            _ => panic!("expected NewEpoch"),
        };
        let leader = match msg {
            Message::NewEpoch(payload) => payload.sender,
            _ => unreachable!(),
        };

        // Phase A2: AckEpoch.
        let ack_epoch_round = match self.rounds.current().get_enum::<_, PhaseA>(Key::PhaseA) {
            PhaseA::NewEpoch => self.rounds.advance(Key::PhaseA),
            PhaseA::AckEpoch => self.rounds.current(),
            other => panic!("unexpected PhaseA in AckEpoch: {:?}", other),
        };

        // Followers send AckEpoch to leader.
        if self.me != leader {
            let ack = AckEpochMsg {
                epoch,
                sender: self.me,
                log: self.log.clone(),
            };
            comm_close::send(leader, Message::AckEpoch(ack));
        }

        // Leader: collect acks and, if quorum, choose a log and install as leader.
        if self.me == leader {
            let quorum = self.nodes.len() / 2;
            let acks = self.collect_ack_epoch(&ack_epoch_round, 0, self.nodes.len());

            if acks.len() <= quorum {
                self.rounds.advance(Key::Epoch);
                return false;
            }

            // Choose the longest log (tie-breaker: most committed).
            let mut best = self.log.clone();
            for ack in &acks {
                if Self::better_log(&ack.log, &best) {
                    best = ack.log.clone();
                }
            }
            self.log = best;

            // Phase A3: NewLeader broadcast.
            let new_leader_round = self.rounds.advance(Key::PhaseA);
            let msg = NewLeaderMsg {
                epoch,
                sender: self.me,
                log: self.log.clone(),
            };
            self.broadcast_with_round(&new_leader_round, Message::NewLeader(msg));

            // Enter broadcast.
            let _bcast_round = self.rounds.advance(Key::PhaseA);
            self.run_broadcast(epoch, leader);
            true
        } else {
            // Follower: wait for a single NewLeader.
            let new_leader_round = self.rounds.advance(Key::PhaseA);
            let leader_msgs = self.collect_new_leader(&new_leader_round, 0, 1);
            if leader_msgs.len() != 1 {
                self.rounds.advance(Key::Epoch);
                return false;
            }

            let (msg, stamp) = &leader_msgs[0];
            self.rounds.jump(stamp);

            match msg {
                Message::NewLeader(payload) => {
                    self.log = payload.log.clone();
                }
                _ => panic!("expected NewLeader"),
            }

            // Enter broadcast.
            let _bcast_round = match self.rounds.current().get_enum::<_, PhaseA>(Key::PhaseA) {
                PhaseA::NewLeader => self.rounds.advance(Key::PhaseA),
                PhaseA::Bcast => self.rounds.current(),
                other => panic!("unexpected PhaseA entering Bcast: {:?}", other),
            };

            self.run_broadcast(epoch, leader);
            true
        }
    }

    fn run_broadcast(&mut self, epoch: u32, leader: ThreadId) {
        // Start from end of installed log.
        let start_slot = u32::try_from(self.log.len()).unwrap_or(u32::MAX);
        self.rounds.goto_u32(Key::Slot, start_slot);

        for _ in 0..self.slots_per_epoch {
            let r = self.rounds.current();
            let slot = r.get_u32(Key::Slot);

            match r.get_enum::<_, PhaseB>(Key::PhaseB) {
                PhaseB::Propose => (),
                PhaseB::Ack | PhaseB::Commit => {
                    // If we ever got here (e.g. by a jump), normalize back to Propose by advancing slot.
                    self.rounds.advance(Key::Slot);
                    continue;
                }
            }

            if self.me == leader {
                // FIRST_ROUND: Propose.
                let value = (0..=MAX_VALUE as usize).nondet() as u32;
                self.ensure_slot_value(slot, value);

                let propose = ProposeMsg {
                    epoch,
                    slot,
                    value,
                    sender: self.me,
                };
                self.broadcast(Message::Propose(propose));

                // SECOND_ROUND: Ack collection.
                let ack_round = self.rounds.advance(Key::PhaseB);
                // Count the leader's own ack (as in the C spec).
                let self_ack = AckMsg {
                    epoch,
                    slot,
                    sender: self.me,
                };
                comm_close::send(leader, Message::Ack(self_ack));

                let quorum = self.nodes.len() / 2;
                let acks = self.collect_ack(&ack_round, 0, self.nodes.len());
                if acks.len() <= quorum {
                    // Lost quorum -> new epoch.
                    self.rounds.advance(Key::Epoch);
                    break;
                }

                // THIRD_ROUND: Commit broadcast.
                self.mark_committed(slot);

                let commit_round = self.rounds.advance(Key::PhaseB);
                let commit = CommitMsg {
                    epoch,
                    slot,
                    value,
                    sender: self.me,
                };
                self.broadcast_with_round(&commit_round, Message::Commit(commit));

                // Next slot.
                self.rounds.advance(Key::Slot);
            } else {
                // Follower: wait for a Propose from the leader. Allow slot >= current (catch-up).
                let propose_round = self.rounds.current();
                let proposes = self.collect_propose(&propose_round, 0, self.nodes.len());
                let chosen = proposes
                    .into_iter()
                    .find(|(m, _)| matches!(m, Message::Propose(p) if p.sender == leader));

                let Some((msg, stamp)) = chosen else {
                    // Timeout -> new epoch.
                    self.rounds.advance(Key::Epoch);
                    break;
                };

                let (slot, value) = match msg {
                    Message::Propose(payload) => (payload.slot, payload.value),
                    _ => panic!("expected Propose"),
                };

                // Jump to the slot we observed (possibly future).
                self.rounds.jump(&stamp);
                self.ensure_slot_value(slot, value);

                // SECOND_ROUND: send Ack to leader.
                let ack_round = self.rounds.advance(Key::PhaseB);
                let ack = AckMsg {
                    epoch,
                    slot,
                    sender: self.me,
                };
                comm_close::send(leader, Message::Ack(ack));

                // THIRD_ROUND: wait for Commit.
                let commit_round = self.rounds.advance(Key::PhaseB);
                let commits = self.collect_commit(&commit_round, 0, self.nodes.len());
                let got_commit = commits
                    .into_iter()
                    .find(|(m, _)| matches!(m, Message::Commit(c) if c.sender == leader && c.slot == slot));

                if let Some((msg, stamp)) = got_commit {
                    self.rounds.jump(&stamp);
                    match msg {
                        Message::Commit(payload) => {
                            self.ensure_slot_value(payload.slot, payload.value);
                            self.mark_committed(payload.slot);
                        }
                        _ => unreachable!(),
                    }
                    self.rounds.advance(Key::Slot);
                } else {
                    self.rounds.advance(Key::Epoch);
                    break;
                }
            }
        }
    }

    fn next_epoch_round(&mut self) -> comm_close::Round {
        if self.started {
            self.rounds.advance(Key::Epoch)
        } else {
            self.started = true;
            self.rounds.current()
        }
    }

    fn broadcast(&self, msg: Message) {
        for node in self.nodes.iter() {
            comm_close::send(*node, msg.clone());
        }
    }

    fn broadcast_with_round(&self, _round: &comm_close::Round, msg: Message) {
        // send() always uses the current round; this helper exists to mirror the example style.
        self.broadcast(msg);
    }

    fn ensure_slot_value(&mut self, slot: u32, value: u32) {
        let idx = usize::try_from(slot).unwrap_or(usize::MAX);
        while self.log.len() <= idx {
            self.log.push(LogEntry {
                value: 0,
                committed: false,
            });
        }
        self.log[idx].value = value;
    }

    fn mark_committed(&mut self, slot: u32) {
        let idx = usize::try_from(slot).unwrap_or(usize::MAX);
        if idx < self.log.len() {
            self.log[idx].committed = true;
        }
    }

    fn better_log(candidate: &[LogEntry], current: &[LogEntry]) -> bool {
        if candidate.len() > current.len() {
            return true;
        }
        if candidate.len() < current.len() {
            return false;
        }
        let cand_committed = candidate.iter().filter(|e| e.committed).count();
        let curr_committed = current.iter().filter(|e| e.committed).count();
        cand_committed > curr_committed
    }

    fn collect_new_epoch(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Epoch, DefaultMatch::Gte)
            .level_cmp(Key::PhaseA, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .filter(|(m, _)| matches!(m, Message::NewEpoch(_)))
            .collect()
    }

    fn collect_ack_epoch(&self, round: &comm_close::Round, min: usize, max: usize) -> Vec<AckEpochMsg> {
        let filter = round
            .filter()
            .level_cmp(Key::Epoch, DefaultMatch::Eq)
            .level_cmp(Key::PhaseA, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .map(|(m, _)| match m {
                Message::AckEpoch(p) => p,
                _ => panic!("expected AckEpoch"),
            })
            .collect()
    }

    fn collect_new_leader(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Epoch, DefaultMatch::Eq)
            .level_cmp(Key::PhaseA, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .filter(|(m, _)| matches!(m, Message::NewLeader(_)))
            .collect()
    }

    fn collect_propose(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Epoch, DefaultMatch::Eq)
            .level_cmp(Key::PhaseA, DefaultMatch::Eq)
            .level_cmp(Key::Slot, DefaultMatch::Gte)
            .level_cmp(Key::PhaseB, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .filter(|(m, _)| matches!(m, Message::Propose(_)))
            .collect()
    }

    fn collect_ack(&self, round: &comm_close::Round, min: usize, max: usize) -> Vec<AckMsg> {
        let filter = round
            .filter()
            .level_cmp(Key::Epoch, DefaultMatch::Eq)
            .level_cmp(Key::PhaseA, DefaultMatch::Eq)
            .level_cmp(Key::Slot, DefaultMatch::Eq)
            .level_cmp(Key::PhaseB, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .map(|(m, _)| match m {
                Message::Ack(p) => p,
                _ => panic!("expected Ack"),
            })
            .collect()
    }

    fn collect_commit(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Epoch, DefaultMatch::Eq)
            .level_cmp(Key::PhaseA, DefaultMatch::Eq)
            .level_cmp(Key::Slot, DefaultMatch::Eq)
            .level_cmp(Key::PhaseB, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .filter(|(m, _)| matches!(m, Message::Commit(_)))
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

fn start_node(
    scheme: RoundScheme,
    max_epochs: u32,
    slots_per_epoch: u32,
    mode: ReceiveMode,
) -> Vec<LogEntry> {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, scheme, max_epochs, slots_per_epoch, mode).run()
}

fn assert_committed_prefix_consistency(logs: &[Vec<LogEntry>]) {
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

fn run_protocol(
    num_nodes: usize,
    max_epochs: u32,
    slots_per_epoch: u32,
    mode: ReceiveMode,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let scheme = RoundScheme::builder()
            .from_u32(Key::Epoch)
            .from_enum::<PhaseA>(Key::PhaseA)
            .from_u32(Key::Slot)
            .from_enum::<PhaseB>(Key::PhaseB)
            .build();

        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            let scheme = scheme.clone();
            let mode = mode;
            handles.push(thread::spawn(move || start_node(scheme, max_epochs, slots_per_epoch, mode)));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(nodes.clone()));
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }

        assert_committed_prefix_consistency(&logs);
    })
}

fn parse_args() -> (usize, u32, u32, ReceiveMode) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut epochs = DEFAULT_NUM_EPOCHS;
    let mut slots = DEFAULT_SLOTS_PER_EPOCH;
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
        } else if arg == "--epochs" {
            let value = args.next().unwrap_or_else(|| panic!("--epochs requires a value"));
            epochs = value.parse().unwrap_or_else(|_| panic!("invalid --epochs value: {}", value));
        } else if arg == "--slots" {
            let value = args.next().unwrap_or_else(|| panic!("--slots requires a value"));
            slots = value.parse().unwrap_or_else(|_| panic!("invalid --slots value: {}", value));
        } else {
            panic!("unknown argument: {}", arg);
        }
    }

    let mode = mode.unwrap_or_else(|| panic!("Must specify recv or inbox!"));
    (num_nodes, epochs, slots, mode)
}

fn main() {
    let (num_nodes, epochs, slots, mode) = parse_args();
    let stats = run_protocol(num_nodes, epochs, slots, mode);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
