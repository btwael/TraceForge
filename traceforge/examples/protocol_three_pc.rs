use traceforge::comm_close::{self, DefaultFilter, DefaultMatch, RoundScheme, Rounds};
use traceforge::thread;
use traceforge::Nondet;
use traceforge::thread::ThreadId;

const DEFAULT_NUM_PARTICIPANTS: usize = 3;
const DEFAULT_NUM_ROUNDS: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundKey)]
enum Key {
    Round,
    Phase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::RoundEnum)]
enum Phase {
    Prepare,
    PreCommit,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vote {
    Yes,
    No,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Commit,
    Abort,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrepareMsg {
    coordinator: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VoteMsg {
    vote: Vote,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreCommitMsg {
    coordinator: ThreadId,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AckMsg {
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DecideMsg {
    decision: Decision,
    sender: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InitMsg {
    coordinator: ThreadId,
    participants: Participants,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Message {
    Init(InitMsg),
    Prepare(PrepareMsg),
    Vote(VoteMsg),
    PreCommit(PreCommitMsg),
    Ack(AckMsg),
    Decide(DecideMsg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Coordinator,
    Participant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveMode {
    Recv,
    Inbox,
}

struct Node {
    role: Role,
    participants: Participants,
    coordinator: ThreadId,
    me: ThreadId,
    rounds: Rounds,
    num_rounds: u32,
    mode: ReceiveMode,
}

impl Node {
    fn new(
        init: InitMsg,
        role: Role,
        scheme: RoundScheme,
        num_rounds: u32,
        mode: ReceiveMode,
    ) -> Self {
        let me = thread::current().id();
        let rounds = Rounds::with_scheme(scheme);
        Self {
            role,
            participants: init.participants,
            coordinator: init.coordinator,
            me,
            rounds,
            num_rounds,
            mode,
        }
    }

    fn run(mut self) {
        match self.role {
            Role::Coordinator => self.run_coordinator(),
            Role::Participant => self.run_participant(),
        }
    }

    fn run_coordinator(&mut self) {
        let participants = self.participants.clone();
        let me = self.me;
        let num_rounds = self.num_rounds;
        let mode = self.mode;

        for _ in 0..num_rounds {
            let r = self.rounds.current();
            let prepare = PrepareMsg {
                coordinator: me,
                sender: me,
            };
            for node in participants.iter() {
                comm_close::send(*node, Message::Prepare(prepare.clone()));
            }

            let votes = collect_votes(mode, &r, participants.len());
            let all_yes = votes.len() == participants.len()
                && votes.iter().all(|msg| msg.vote == Vote::Yes);
            if !all_yes {
                self.rounds.advance(Key::Phase);
                let decide = DecideMsg {
                    decision: Decision::Abort,
                    sender: me,
                };
                for node in participants.iter() {
                    comm_close::send(*node, Message::Decide(decide.clone()));
                }
                self.rounds.advance_round();
                continue;
            }

            self.rounds.advance(Key::Phase);
            let precommit = PreCommitMsg {
                coordinator: me,
                sender: me,
            };
            for node in participants.iter() {
                comm_close::send(*node, Message::PreCommit(precommit.clone()));
            }

            let crash_after_precommit = traceforge::nondet();
            if crash_after_precommit {
                return;
            }

            let r = self.rounds.current();
            let acks = collect_acks(mode, &r, participants.len());
            if acks.len() != participants.len() {
                // Timeout waiting for all acks: do not commit this round.
                self.rounds.advance_round();
                continue;
            }

            self.rounds.advance(Key::Phase);
            let decide = DecideMsg {
                decision: Decision::Commit,
                sender: me,
            };
            for node in participants.iter() {
                comm_close::send(*node, Message::Decide(decide.clone()));
            }

            self.rounds.advance_round();
        }
    }

    fn run_participant(&mut self) {
        let coordinator = self.coordinator;
        let me = self.me;
        let num_rounds = self.num_rounds;
        let mode = self.mode;

        for _ in 0..num_rounds {
            let r = self.rounds.current();
            let _prepare = collect_prepare(&r);

            let crash_before_vote = traceforge::nondet();
            if crash_before_vote {
                return;
            }

            let vote_yes = traceforge::nondet();
            let vote = if vote_yes { Vote::Yes } else { Vote::No };
            let vote_msg = VoteMsg { vote, sender: me };
            comm_close::send(coordinator, Message::Vote(vote_msg));

            self.rounds.advance(Key::Phase);
            let r = self.rounds.current();
            let msg = collect_precommit_or_abort(&r);
            if msg.is_none() {
                // Coordinator crash before PreCommit: abort without blocking.
                return;
            }
            let msg = msg.expect("checked above");

            match msg {
                Message::Decide(decide) => {
                    assert_eq!(decide.decision, Decision::Abort);
                    self.rounds.advance_round();
                    continue;
                }
                Message::PreCommit(_) => {
                    if !vote_yes {
                        panic!("received PreCommit after voting No");
                    }
                }
                _ => panic!("expected PreCommit or Decide(Abort)"),
            }

            let ack = AckMsg { sender: me };
            comm_close::send(coordinator, Message::Ack(ack));

            self.rounds.advance(Key::Phase);
            let r = self.rounds.current();
            let decision = collect_optional_decide(mode, &r);

            let decided = decision.is_some();
            match decision {
                Some(decide) => {
                    assert_eq!(decide.decision, Decision::Commit);
                }
                None => {
                    // Coordinator crash after PreCommit: complete without blocking.
                }
            }

            self.rounds.advance_round();

            if !decided {
                return;
            }
        }
    }
}

fn collect_prepare(round: &comm_close::Round) -> PrepareMsg {
    let msg = comm_close::recv_block::<Message>(round);
    match msg.payload(round) {
        Message::Prepare(payload) => payload.clone(),
        _ => panic!("expected PrepareMsg"),
    }
}

fn collect_votes(mode: ReceiveMode, round: &comm_close::Round, expected: usize) -> Vec<VoteMsg> {
    let messages = match mode {
        ReceiveMode::Recv => collect_bounded_recv(round, 0, expected),
        ReceiveMode::Inbox => collect_bounded_inbox(round, 0, expected),
    };
    messages
        .into_iter()
        .filter_map(|msg| match msg {
            Message::Vote(payload) => Some(payload),
            _ => panic!("expected VoteMsg"),
        })
        .collect()
}

fn collect_acks(mode: ReceiveMode, round: &comm_close::Round, expected: usize) -> Vec<AckMsg> {
    match mode {
        ReceiveMode::Recv => {
            let count = if expected == 0 {
                0
            } else {
                (0..=expected).nondet()
            };
            let mut out = Vec::new();
            for _ in 0..count {
                let msg = comm_close::recv_block::<Message>(round);
                match msg.payload(round) {
                    Message::Ack(payload) => out.push(payload.clone()),
                    _ => panic!("expected AckMsg"),
                }
            }
            out
        }
        ReceiveMode::Inbox => {
            let filter = round.filter();
            let msgs = comm_close::inbox_with_bounds_filter(&filter, 0, Some(expected));
            let mut out = Vec::new();
            for msg in msgs.into_iter().flatten() {
                let payload = msg
                    .payload(round)
                    .as_any_ref()
                    .downcast_ref::<Message>()
                    .cloned()
                    .expect("expected Message payload");
                match payload {
                    Message::Ack(payload) => out.push(payload),
                    _ => panic!("expected AckMsg"),
                }
            }
            out
        }
    }
}

fn collect_precommit_or_abort(round: &comm_close::Round) -> Option<Message> {
    let msg = comm_close::recv::<Message>(round)?;
    let msg = msg.payload(round).clone();
    match msg {
        Message::PreCommit(_) => Some(msg),
        Message::Decide(payload) => {
            assert_eq!(payload.decision, Decision::Abort);
            Some(Message::Decide(payload))
        }
        _ => panic!("expected PreCommit or Decide(Abort)"),
    }
}

fn collect_optional_decide(mode: ReceiveMode, round: &comm_close::Round) -> Option<DecideMsg> {
    match mode {
        ReceiveMode::Recv => {
            let msg = comm_close::recv::<Message>(round);
            msg.map(|msg| match msg.payload(round) {
                Message::Decide(payload) => payload.clone(),
                _ => panic!("expected DecideMsg"),
            })
        }
        ReceiveMode::Inbox => {
            let filter = round.filter();
            let msgs = comm_close::inbox_with_bounds_filter(&filter, 0, Some(1));
            for msg in msgs.into_iter().flatten() {
                let payload = msg
                    .payload(round)
                    .as_any_ref()
                    .downcast_ref::<Message>()
                    .cloned()
                    .expect("expected Message payload");
                match payload {
                    Message::Decide(decide) => return Some(decide),
                    _ => panic!("expected DecideMsg"),
                }
            }
            None
        }
    }
}

fn collect_exact_recv(round: &comm_close::Round, expected: usize) -> Vec<Message> {
    let filter = round.filter();
    let mut out = Vec::new();
    for _ in 0..expected {
        let msg = comm_close::recv_block_with_filter::<Message>(&filter);
        out.push(msg.payload(round).clone());
    }
    out
}

fn collect_exact_inbox(round: &comm_close::Round, expected: usize) -> Vec<Message> {
    if expected == 1 {
        let msg = comm_close::recv_block::<Message>(round);
        return vec![msg.payload(round).clone()];
    }
    let filter = round.filter();
    let msgs = comm_close::inbox_with_bounds_filter(&filter, expected, Some(expected));
    let mut out = Vec::new();
    for msg in msgs.into_iter().flatten() {
        let payload = msg
            .payload(round)
            .as_any_ref()
            .downcast_ref::<Message>()
            .cloned()
            .expect("expected Message payload");
        out.push(payload);
    }
    if out.len() != expected {
        panic!("expected {} messages, got {}", expected, out.len());
    }
    out
}

fn collect_bounded_recv(round: &comm_close::Round, min: usize, max: usize) -> Vec<Message> {
    assert!(max >= min, "requires max >= min");
    let filter = round.filter();
    let count = if max == min {
        min
    } else {
        (min..=max).nondet()
    };
    let mut out = Vec::new();
    for _ in 0..count {
        let msg = match comm_close::recv_with_filter::<Message>(&filter) {
            Some(msg) => msg,
            None => break,
        };
        out.push(msg.payload(round).clone());
    }
    out
}

fn collect_bounded_inbox(round: &comm_close::Round, min: usize, max: usize) -> Vec<Message> {
    assert!(max >= min, "requires max >= min");
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
        out.push(payload);
    }
    out
}

fn start_node(role: Role, scheme: RoundScheme, num_rounds: u32, mode: ReceiveMode) {
    let init: Message = traceforge::recv_tagged_msg_block(|_, tag| tag.is_none());
    let init = match init {
        Message::Init(init) => init,
        _ => panic!("expected init message"),
    };
    Node::new(init, role, scheme, num_rounds, mode).run();
}

fn run_protocol(mode: ReceiveMode, num_participants: usize, num_rounds: u32) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let scheme = RoundScheme::builder()
            .default_filter(
                DefaultFilter::new()
                    .level_match(Key::Round, DefaultMatch::Eq)
                    .level_match(Key::Phase, DefaultMatch::Eq),
            )
            .from_u32(Key::Round)
            .from_enum::<Phase>(Key::Phase)
            .build();

        let mut handles = Vec::new();

        let coordinator_handle = thread::spawn({
            let scheme = scheme.clone();
            let num_rounds = num_rounds;
            let mode = mode;
            move || start_node(Role::Coordinator, scheme, num_rounds, mode)
        });
        let coordinator = coordinator_handle.thread().id();
        handles.push(coordinator_handle);

        let mut participant_ids = Vec::new();
        for _ in 0..num_participants {
            let scheme = scheme.clone();
            let num_rounds = num_rounds;
            let mode = mode;
            let handle =
                thread::spawn(move || start_node(Role::Participant, scheme, num_rounds, mode));
            participant_ids.push(handle.thread().id());
            handles.push(handle);
        }

        let participants = Participants::from_vec(participant_ids);
        let init = InitMsg {
            coordinator,
            participants,
        };
        for handle in &handles {
            traceforge::send_msg(handle.thread().id(), Message::Init(init.clone()));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    })
}

fn run_protocol_with_recv(num_participants: usize, num_rounds: u32) -> traceforge::Stats {
    run_protocol(ReceiveMode::Recv, num_participants, num_rounds)
}

fn run_protocol_with_inbox(num_participants: usize, num_rounds: u32) -> traceforge::Stats {
    run_protocol(ReceiveMode::Inbox, num_participants, num_rounds)
}

fn parse_args() -> (usize, u32, ReceiveMode) {
    let mut num_participants = DEFAULT_NUM_PARTICIPANTS;
    let mut num_rounds = DEFAULT_NUM_ROUNDS;
    let mut mode: Option<ReceiveMode> = None;
    let mut args = std::env::args().skip(1).peekable();

    while let Some(arg) = args.next() {
        if arg == "recv" {
            mode = Some(ReceiveMode::Recv);
        } else if arg == "inbox" {
            mode = Some(ReceiveMode::Inbox);
        } else if arg == "--participants" {
            let value = args
                .next()
                .unwrap_or_else(|| panic!("--participants requires a value"));
            num_participants = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid --participants value: {}", value));
        } else if arg == "--rounds" {
            let value = args
                .next()
                .unwrap_or_else(|| panic!("--rounds requires a value"));
            num_rounds = value
                .parse()
                .unwrap_or_else(|_| panic!("invalid --rounds value: {}", value));
        } else {
            panic!("unknown argument: {}", arg);
        }
    }

    let mode = mode.unwrap_or_else(|| panic!("Must specify recv or inbox!"));
    (num_participants, num_rounds, mode)
}

fn main() {
    let (num_participants, num_rounds, mode) = parse_args();

    let stats = match mode {
        ReceiveMode::Recv => run_protocol_with_recv(num_participants, num_rounds),
        ReceiveMode::Inbox => run_protocol_with_inbox(num_participants, num_rounds),
    };
    println!("Stats = {}, {}", stats.execs, stats.block);
}
