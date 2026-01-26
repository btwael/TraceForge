use std::sync::Arc;

use traceforge::comm_close::{self, RoundScheme, RoundStamp, Rounds, TagCmp};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

// Keep bounds explicit and small for verification.
const NUM_NODES: usize = 3;
const NUM_ROUNDS: u32 = 2;

// Two-Phase Commit requires unanimity.
const ALL: usize = NUM_NODES;

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
    Aux,
    First,  // coordinator sends command
    Second, // participants vote
    Third,  // coordinator broadcasts decision
    Fourth, // participants ack
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FirstPhaseMsg {
    stamp: RoundStamp,
    command: i32,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SecondPhaseMsg {
    stamp: RoundStamp,
    command: i32,
    vote: bool,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThirdPhaseMsg {
    stamp: RoundStamp,
    command: i32,
    commit: bool,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FourthPhaseMsg {
    stamp: RoundStamp,
    command: i32,
    vote: bool,
    commit: bool,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    First(FirstPhaseMsg),
    Second(SecondPhaseMsg),
    Third(ThirdPhaseMsg),
    Fourth(FourthPhaseMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogEntry {
    round: u32,
    command: i32,
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

    // Per-round state.
    command: i32,
    vote: bool,
    commit: bool,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);

        Self {
            nodes,
            me,
            rounds,
            command: 1,
            vote: false,
            commit: false,
        }
    }

    fn run(mut self, collect: &RoundCollector) -> Vec<LogEntry> {
        let mut log = Vec::new();

        // We start at (round=0, phase=Aux) and enter the first phase by advancing one level.
        for _ in 0..NUM_ROUNDS {
            self.step_round(collect, &mut log);
        }
        log
    }

    fn step_round(&mut self, collect: &RoundCollector, log: &mut Vec<LogEntry>) {
        // Loop entry is AUX for the current round.
        let _aux = self.rounds.current();
        let first_phase = self.rounds.advance_level(1);
        let round_id = self.round_of(&first_phase.stamp());

        // Coordinator for this outer round.
        let leader = self.leader_for_round(round_id);

        // Reset per-round state.
        self.command = 1;
        self.vote = false;
        self.commit = false;

        // --- Phase 1: leader broadcasts a command; everyone picks a vote after learning it ---
        if self.me == leader {
            self.command = Self::input_command();
            self.vote = Self::rand_bool();

            let msg = FirstPhaseMsg {
                stamp: first_phase.stamp(),
                command: self.command,
                sender: self.me,
            };
            self.broadcast(&first_phase, Message::First(msg));
        } else {
            let msgs = self.collect_first_phase(&first_phase, collect, 1, Some(1));
            let msg = &msgs[0];

            self.command = msg.command;
            self.vote = Self::rand_bool();
        }

        // --- Phase 2: everyone sends vote to leader; leader collects all votes and decides ---
        let second_phase = self.rounds.advance_level(1);

        if self.me != leader {
            let msg = SecondPhaseMsg {
                stamp: second_phase.stamp(),
                command: self.command,
                vote: self.vote,
                sender: self.me,
            };
            comm_close::send(leader, Message::Second(msg), &second_phase);
        }

        if self.me == leader {
            let mut votes = vec![SecondPhaseMsg {
                stamp: second_phase.stamp(),
                command: self.command,
                vote: self.vote,
                sender: self.me,
            }];

            let mut recvd = self.collect_second_phase(
                &second_phase,
                collect,
                ALL.saturating_sub(1),
                Some(ALL.saturating_sub(1)),
            );
            votes.append(&mut recvd);

            self.commit = Self::all_yes(&votes);
        }

        // --- Phase 3: leader broadcasts decision; everyone commits/aborts ---
        let third_phase = self.rounds.advance_level(1);

        if self.me == leader {
            let msg = ThirdPhaseMsg {
                stamp: third_phase.stamp(),
                command: self.command,
                commit: self.commit,
                sender: self.me,
            };
            self.broadcast(&third_phase, Message::Third(msg));

            if self.commit {
                self.out(round_id, self.command, log);
            }
        } else {
            let msgs = self.collect_third_phase(&third_phase, collect, 1, Some(1));
            let msg = &msgs[0];

            // We keep our locally-learned command (from phase 1) but record the decision.
            self.commit = msg.commit;

            if msg.commit {
                self.out(round_id, self.command, log);
            }
        }

        // --- Phase 4: everyone acks to leader; leader waits for all acks then advances ---
        let fourth_phase = self.rounds.advance_level(1);

        if self.me != leader {
            let msg = FourthPhaseMsg {
                stamp: fourth_phase.stamp(),
                command: self.command,
                vote: self.vote,
                commit: self.commit,
                sender: self.me,
            };
            comm_close::send(leader, Message::Fourth(msg), &fourth_phase);
        }

        if self.me == leader {
            let mut acks = vec![FourthPhaseMsg {
                stamp: fourth_phase.stamp(),
                command: self.command,
                vote: self.vote,
                commit: self.commit,
                sender: self.me,
            }];

            let mut recvd = self.collect_fourth_phase(
                &fourth_phase,
                collect,
                ALL.saturating_sub(1),
                Some(ALL.saturating_sub(1)),
            );
            acks.append(&mut recvd);

            // Barrier only: the C code doesn't inspect acks.
            let _ = acks;
        }

        // Advance to next outer round; phase resets to AUX.
        self.rounds.advance_round();
    }

    fn input_command() -> i32 {
        // Mirror the C code's "payload > 0" guard by always producing a positive command.
        if traceforge::nondet() { 1 } else { 2 }
    }

    fn rand_bool() -> bool {
        traceforge::nondet()
    }

    fn round_of(&self, stamp: &RoundStamp) -> u32 {
        stamp.components()[0]
    }

    fn leader_for_round(&self, round_id: u32) -> ThreadId {
        let idx = (round_id as usize) % self.nodes.len();
        self.nodes.get(idx)
    }

    fn broadcast(&self, round: &comm_close::Round, msg: Message) {
        for node in self.nodes.iter() {
            if *node != self.me {
                comm_close::send(*node, msg.clone(), round);
            }
        }
    }

    fn out(&self, round_id: u32, command: i32, log: &mut Vec<LogEntry>) {
        log.push(LogEntry {
            round: round_id,
            command,
        });
    }

    fn collect_first_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<FirstPhaseMsg> {
        // Exact outer-round match (do not accept future-round Phase1 messages).
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::First(payload) => Some(payload),
                _ => panic!("expected FirstPhaseMsg"),
            })
            .collect()
    }

    fn collect_second_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<SecondPhaseMsg> {
        // Exact outer-round match.
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Second(payload) => Some(payload),
                _ => panic!("expected SecondPhaseMsg"),
            })
            .collect()
    }

    fn collect_third_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<ThirdPhaseMsg> {
        // Exact outer-round match.
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Third(payload) => Some(payload),
                _ => panic!("expected ThirdPhaseMsg"),
            })
            .collect()
    }

    fn collect_fourth_phase(
        &self,
        round: &comm_close::Round,
        collect: &RoundCollector,
        min: usize,
        max: Option<usize>,
    ) -> Vec<FourthPhaseMsg> {
        // Exact outer-round match.
        let filter = round.filter().level_cmp(0, TagCmp::Eq);
        collect(round, &filter, min, max)
            .into_iter()
            .filter_map(|msg| match msg {
                Message::Fourth(payload) => Some(payload),
                _ => panic!("expected FourthPhaseMsg"),
            })
            .collect()
    }

    fn all_yes(votes: &[SecondPhaseMsg]) -> bool {
        votes.iter().all(|m| m.vote)
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

fn assert_atomicity(logs: &[Vec<LogEntry>]) {
    // For each round: either nobody outputs (abort), or everybody outputs the same command (commit).
    for r in 0..NUM_ROUNDS {
        let mut cmds = Vec::new();
        for log in logs {
            for entry in log {
                if entry.round == r {
                    cmds.push(entry.command);
                }
            }
        }

        if cmds.is_empty() {
            continue;
        }

        if cmds.len() != NUM_NODES {
            panic!(
                "atomicity violated: round {} has {} commits (expected 0 or {})",
                r,
                cmds.len(),
                NUM_NODES
            );
        }

        let first = cmds[0];
        if cmds.iter().any(|c| *c != first) {
            panic!(
                "atomicity violated: round {} committed different commands: {:?}",
                r, cmds
            );
        }
    }
}

fn run_protocol(collect: Arc<RoundCollector>) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        // Tag layout (outer -> inner): (round, phase).
        // - round: grows monotonically, so we use Gte by default;
        // - phase: bounded step tag (Aux..Fourth), so we use Eq by default.
        let scheme = RoundScheme::builder()
            .level("round", TagCmp::Gte)
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
        assert_atomicity(&logs);
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
