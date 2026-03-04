
use traceforge::comm_close::{self, DefaultMatch, RoundScheme, RoundStamp, Rounds};
use traceforge::thread::ThreadId;
use traceforge::{thread, Nondet};

const DEFAULT_NUM_NODES: usize = 3;
const DEFAULT_NUM_BALLOTS: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundKey)]
enum Key {
    Ballot,
    Round,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum RoundTyp {
    NewBallot,
    AckBallot,
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

    fn index_of(&self, tid: ThreadId) -> usize {
        self.nodes
            .iter()
            .position(|t| *t == tid)
            .unwrap_or_else(|| panic!("thread {:?} not in participants", tid))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NewBallotMsg {
    ballot: u32,
    leader: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AckBallotMsg {
    ballot: u32,
    leader: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(Participants),
    NewBallot(NewBallotMsg),
    AckBallot(AckBallotMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogEntry {
    ballot: u32,
    leader: ThreadId,
}

struct Node {
    nodes: Participants,
    me: ThreadId,
    rounds: Rounds,
    started: bool,
    mode: ReceiveMode,

    leader: ThreadId,
    num_ballots: u32,
}

impl Node {
    fn new(nodes: Participants, scheme: RoundScheme, num_ballots: u32, mode: ReceiveMode) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
        Self {
            nodes,
            me,
            rounds,
            started: false,
            mode,
            leader: me,
            num_ballots,
        }
    }

    fn run(mut self) -> Vec<LogEntry> {
        let mut log = Vec::new();
        for _ in 0..self.num_ballots {
            self.step(&mut log);
        }
        log
    }

    fn step(&mut self, log: &mut Vec<LogEntry>) {
        // Start (or advance) to next ballot; round is reset to NewBallot by the scheme.
        let nb_round = self.next_round();
        let ballot = nb_round.get_u32(Key::Ballot);

        // Deterministic Paxos-like coordinator per ballot (ballot mod n).
        if self.coord() {
            let msg = NewBallotMsg {
                ballot,
                leader: self.me,
                sender: self.me,
            };
            self.broadcast(Message::NewBallot(msg));
        }

        // Reception loop (modelled with bounded recv/inbox) for NewBallot_ROUND:
        // accept only NewBallot messages, but allow ballot >= current ballot.
        let nb_msgs = self.collect_new_ballot(&nb_round, 0, 1);

        // In the C version, we proceed only when we observed a NewBallot.
        if nb_msgs.len() != 1 {
            return;
        }

        let (msg, stamp) = &nb_msgs[0];
        // Jump to the (possibly future) ballot we observed.
        self.rounds.jump(stamp);

        match msg {
            Message::NewBallot(payload) => {
                self.leader = payload.leader;
            }
            _ => panic!("expected NewBallot"),
        }

        // AckBallot_ROUND.
        let ack_round = self.rounds.advance(Key::Round);
        let ballot = ack_round.get_u32(Key::Ballot);

        let ack = AckBallotMsg {
            ballot,
            leader: self.leader,
            sender: self.me,
        };
        self.broadcast(Message::AckBallot(ack));

        let recv_max = self.nodes.len();
        let quorum = self.nodes.len() / 2;
        let acks = self.collect_ack_ballot(&ack_round, 0, recv_max);

        if acks.len() > quorum && Self::all_same_leader(&acks, self.leader) {
            log.push(LogEntry {
                ballot,
                leader: self.leader,
            });
        }
    }

    fn coord(&self) -> bool {
        traceforge::nondet()
    }

    fn next_round(&mut self) -> comm_close::Round {
        if self.started {
            self.rounds.advance(Key::Ballot)
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

    fn collect_new_ballot(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(Message, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Ballot, DefaultMatch::Gte)
            .level_cmp(Key::Round, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .filter(|(m, _)| matches!(m, Message::NewBallot(_)))
            .collect()
    }

    fn collect_ack_ballot(
        &self,
        round: &comm_close::Round,
        min: usize,
        max: usize,
    ) -> Vec<(AckBallotMsg, RoundStamp)> {
        let filter = round
            .filter()
            .level_cmp(Key::Ballot, DefaultMatch::Eq)
            .level_cmp(Key::Round, DefaultMatch::Eq);
        self.collect_messages_with_filter(round, &filter, min, max)
            .into_iter()
            .map(|(msg, stamp)| match msg {
                Message::AckBallot(payload) => (payload, stamp),
                _ => panic!("expected AckBallot"),
            })
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

    fn all_same_leader(messages: &[(AckBallotMsg, RoundStamp)], leader: ThreadId) -> bool {
        messages.iter().all(|(msg, _)| msg.leader == leader)
    }
}

fn start_node(scheme: RoundScheme, num_ballots: u32, mode: ReceiveMode) -> Vec<LogEntry> {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let nodes = match init {
        Message::Init(nodes) => nodes,
        _ => panic!("expected init message"),
    };
    Node::new(nodes, scheme, num_ballots, mode).run()
}

fn assert_log_consistency(logs: &[Vec<LogEntry>], num_ballots: u32) {
    for ballot in 0..=num_ballots {
        let mut chosen: Option<ThreadId> = None;
        for log in logs {
            for entry in log.iter().filter(|entry| entry.ballot == ballot) {
                if let Some(prev) = chosen {
                    assert_eq!(prev, entry.leader);
                } else {
                    chosen = Some(entry.leader);
                }
            }
        }
    }
}
fn run_protocol(num_nodes: usize, num_ballots: u32, mode: ReceiveMode) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let scheme = RoundScheme::builder()
            .from_u32(Key::Ballot)
            .from_enum::<RoundTyp>(Key::Round)
            .build(); // default: lexicographic, gte receive

        let mut handles = Vec::new();
        for _ in 0..num_nodes {
            let scheme = scheme.clone();
            let mode = mode;
            let num_ballots = num_ballots;
            handles.push(thread::spawn(move || start_node(scheme, num_ballots, mode)));
        }

        let nodes = Participants::from_vec(handles.iter().map(|h| h.thread().id()).collect());
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(nodes.clone()));
        }

        let mut logs = Vec::new();
        for handle in handles {
            logs.push(handle.join().unwrap());
        }

        assert_log_consistency(&logs, num_ballots);
    })
}

fn parse_args() -> (usize, u32, ReceiveMode) {
    let mut num_nodes = DEFAULT_NUM_NODES;
    let mut num_ballots = DEFAULT_NUM_BALLOTS;
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
        } else if arg == "--ballots" {
            let value = args.next().unwrap_or_else(|| panic!("--ballots requires a value"));
            num_ballots = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid --ballots value: {}", value));
        } else {
            panic!("unknown argument: {}", arg);
        }
    }

    let mode = mode.unwrap_or_else(|| panic!("Must specify recv or inbox!"));
    (num_nodes, num_ballots, mode)
}

fn main() {
    let (num_nodes, num_ballots, mode) = parse_args();
    let stats = run_protocol(num_nodes, num_ballots, mode);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
