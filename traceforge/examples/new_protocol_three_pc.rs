use traceforge::comm_close::Rounds;
use traceforge::thread;
use traceforge::thread::ThreadId;
use traceforge::Nondet;

const DEFAULT_NUM_PARTICIPANTS: usize = 3;
const DEFAULT_NUM_ROUNDS: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Inbox;
const DEFAULT_USE_TAGS: bool = true;
const INIT_TAG: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum Phase {
    Prepare,
    PreCommit,
    Commit,
}

#[derive(traceforge::Round)]
struct ThreePcRound {
    #[dimension("=")]
    round: u32,
    #[dimension("=")]
    phase: Phase,
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
    rounds: Rounds<ThreePcRound>,
    num_rounds: u32,
    mode: ReceiveMode,
    crashes_enabled: bool,
}

impl Node {
    fn new(
        init: InitMsg,
        role: Role,
        num_rounds: u32,
        mode: ReceiveMode,
        crashes_enabled: bool,
        use_tags: bool,
    ) -> Self {
        let me = thread::current().id();
        let rounds = if use_tags {
            Rounds::<ThreePcRound>::new()
        } else {
            Rounds::<ThreePcRound>::new_wo_tags(false)
        };
        Self {
            role,
            participants: init.participants,
            coordinator: init.coordinator,
            me,
            rounds,
            num_rounds,
            mode,
            crashes_enabled,
        }
    }

    fn run(mut self) {
        match self.role {
            Role::Coordinator => self.run_coordinator(),
            Role::Participant => self.run_participant(),
        }
    }

    fn run_coordinator(&mut self) {
        let me = self.me;
        for _ in 0..self.num_rounds {
            self.broadcast(Message::Prepare(PrepareMsg {
                sender: me,
            }));

            let votes = collect_votes(&self.rounds, self.participants.len(), self.mode);
            let all_yes = votes.len() == self.participants.len()
                && votes.iter().all(|msg| msg.vote == Vote::Yes);

            if !all_yes {
                self.rounds.advance(ThreePcRound::phase());
                self.broadcast(Message::Decide(DecideMsg {
                    decision: Decision::Abort,
                    sender: me,
                }));
                self.rounds.advance(ThreePcRound::round());
                continue;
            }

            self.rounds.advance(ThreePcRound::phase());
            self.broadcast(Message::PreCommit(PreCommitMsg {
                coordinator: me,
                sender: me,
            }));

            if self.crashes_enabled && traceforge::nondet() {
                return;
            }

            let acks = collect_acks(&self.rounds, self.participants.len(), self.mode);
            if acks.len() != self.participants.len() {
                // Timeout waiting for all acks: do not commit this round.
                self.rounds.advance(ThreePcRound::round());
                continue;
            }

            self.rounds.advance(ThreePcRound::phase());
            self.broadcast(Message::Decide(DecideMsg {
                decision: Decision::Commit,
                sender: me,
            }));
            self.rounds.advance(ThreePcRound::round());
        }
    }

    fn run_participant(&mut self) {
        let coordinator = self.coordinator;
        let me = self.me;
        for _ in 0..self.num_rounds {
            let _prepare = collect_prepare(&self.rounds);

            if self.crashes_enabled && traceforge::nondet() {
                return;
            }

            let vote_yes = traceforge::nondet();
            let vote = if vote_yes { Vote::Yes } else { Vote::No };
            self.rounds
                .send(coordinator, Message::Vote(VoteMsg { vote, sender: me }));

            self.rounds.advance(ThreePcRound::phase());
            let msg = collect_precommit_or_abort(&self.rounds);
            if msg.is_none() {
                // Coordinator unavailable before PreCommit/Abort.
                return;
            }
            let msg = msg.expect("checked above");

            match msg {
                Message::Decide(decide) => {
                    assert_eq!(decide.decision, Decision::Abort);
                    self.rounds.advance(ThreePcRound::round());
                    continue;
                }
                Message::PreCommit(_) => {
                    if !vote_yes {
                        panic!("received PreCommit after voting No");
                    }
                }
                _ => panic!("expected PreCommit or Decide(Abort)"),
            }

            self.rounds.send(coordinator, Message::Ack(AckMsg { sender: me }));

            self.rounds.advance(ThreePcRound::phase());
            let decision = collect_optional_decide(&self.rounds, self.mode);
            let decided = decision.is_some();
            if let Some(decide) = decision {
                assert_eq!(decide.decision, Decision::Commit);
            }

            self.rounds.advance(ThreePcRound::round());

            if !decided {
                return;
            }
        }
    }

    fn broadcast(&self, msg: Message) {
        for node in self.participants.iter() {
            self.rounds.send(*node, msg.clone());
        }
    }
}

fn collect_prepare(rounds: &Rounds<ThreePcRound>) -> PrepareMsg {
    let msg = rounds.recv_block::<Message>();
    match msg.payload() {
        Message::Prepare(payload) => payload.clone(),
        _ => panic!("expected PrepareMsg"),
    }
}

fn collect_votes(rounds: &Rounds<ThreePcRound>, expected: usize, mode: ReceiveMode) -> Vec<VoteMsg> {
    let messages = match mode {
        ReceiveMode::Recv => collect_bounded_recv(rounds, 0, expected),
        ReceiveMode::Inbox => collect_bounded_inbox(rounds, 0, expected),
    };
    messages.into_iter().map(expect_vote).collect()
}

fn collect_acks(rounds: &Rounds<ThreePcRound>, expected: usize, mode: ReceiveMode) -> Vec<AckMsg> {
    match mode {
        ReceiveMode::Recv => {
            let count = if expected == 0 {
                0
            } else {
                (0..=expected).nondet()
            };
            let mut out = Vec::new();
            for _ in 0..count {
                let msg = rounds.recv_block::<Message>();
                out.push(expect_ack(msg.payload().clone()));
            }
            out
        }
        ReceiveMode::Inbox => {
            let messages = collect_bounded_inbox(rounds, 0, expected);
            messages.into_iter().map(expect_ack).collect()
        }
    }
}

fn collect_precommit_or_abort(rounds: &Rounds<ThreePcRound>) -> Option<Message> {
    let msg = rounds.recv::<Message>()?;
    let payload = msg.payload().clone();
    match &payload {
        Message::PreCommit(_) => Some(payload),
        Message::Decide(decide) => {
            assert_eq!(decide.decision, Decision::Abort);
            Some(payload)
        }
        _ => panic!("expected PreCommit or Decide(Abort)"),
    }
}

fn collect_optional_decide(rounds: &Rounds<ThreePcRound>, mode: ReceiveMode) -> Option<DecideMsg> {
    match mode {
        ReceiveMode::Recv => {
            let msg = rounds.recv::<Message>()?;
            match msg.payload() {
                Message::Decide(payload) => Some(payload.clone()),
                _ => panic!("expected DecideMsg"),
            }
        }
        ReceiveMode::Inbox => {
            let filter = rounds.filter();
            let msgs = rounds.inbox_with_bounds_with::<Message>(&filter, 0, Some(1));
            for msg in msgs.into_iter().flatten() {
                match msg.payload() {
                    Message::Decide(decide) => return Some(decide.clone()),
                    _ => panic!("expected DecideMsg"),
                }
            }
            None
        }
    }
}

fn collect_bounded_recv(rounds: &Rounds<ThreePcRound>, min: usize, max: usize) -> Vec<Message> {
    assert!(max >= min, "requires max >= min");
    let filter = rounds.filter();
    let count = if max == min {
        min
    } else {
        (min..=max).nondet()
    };
    let mut out = Vec::new();
    for _ in 0..count {
        let msg = match rounds.recv_with::<Message>(&filter) {
            Some(msg) => msg,
            None => break,
        };
        out.push(msg.payload().clone());
    }
    out
}

fn collect_bounded_inbox(rounds: &Rounds<ThreePcRound>, min: usize, max: usize) -> Vec<Message> {
    assert!(max >= min, "requires max >= min");
    let filter = rounds.filter();
    let msgs = rounds.inbox_with_bounds_with::<Message>(&filter, min, Some(max));
    let mut out = Vec::new();
    for msg in msgs.into_iter().flatten() {
        out.push(msg.payload().clone());
    }
    out
}

fn expect_vote(msg: Message) -> VoteMsg {
    match msg {
        Message::Vote(payload) => payload,
        _ => panic!("expected VoteMsg"),
    }
}

fn expect_ack(msg: Message) -> AckMsg {
    match msg {
        Message::Ack(payload) => payload,
        _ => panic!("expected AckMsg"),
    }
}

fn start_node(
    role: Role,
    num_rounds: u32,
    mode: ReceiveMode,
    crashes_enabled: bool,
    use_tags: bool,
) {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let init = match init {
        Message::Init(init) => init,
        _ => panic!("expected init message"),
    };
    Node::new(init, role, num_rounds, mode, crashes_enabled, use_tags).run();
}

fn run_protocol(
    num_participants: usize,
    num_rounds: u32,
    mode: ReceiveMode,
    crashes_enabled: bool,
    use_tags: bool,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let mut handles = Vec::new();

        let coordinator_handle = thread::spawn(move || {
            start_node(Role::Coordinator, num_rounds, mode, crashes_enabled, use_tags)
        });
        let coordinator = coordinator_handle.thread().id();
        handles.push(coordinator_handle);

        let mut participant_ids = Vec::new();
        for _ in 0..num_participants {
            let handle = thread::spawn(move || {
                start_node(Role::Participant, num_rounds, mode, crashes_enabled, use_tags)
            });
            participant_ids.push(handle.thread().id());
            handles.push(handle);
        }

        let participants = Participants::from_vec(participant_ids);
        let init = InitMsg {
            coordinator,
            participants,
        };
        for handle in &handles {
            if use_tags {
                traceforge::send_msg(handle.thread().id(), Message::Init(init.clone()));
            } else {
                traceforge::send_tagged_msg(handle.thread().id(), INIT_TAG, Message::Init(init.clone()));
            }
        }

        for handle in handles {
            handle.join().unwrap();
        }
    })
}

fn parse_args() -> (usize, u32, ReceiveMode, bool, bool) {
    let mut num_participants = DEFAULT_NUM_PARTICIPANTS;
    let mut num_rounds = DEFAULT_NUM_ROUNDS;
    let mut mode = DEFAULT_MODE;
    let mut wo_crashes = false;
    let mut use_tags = DEFAULT_USE_TAGS;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--participants" => {
                let value = next_arg_value(&mut args, "--participants");
                num_participants = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --participants value: {}", value));
            }
            "--rounds" => {
                let value = next_arg_value(&mut args, "--rounds");
                num_rounds = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --rounds value: {}", value));
            }
            "--mode" => {
                let value = next_arg_value(&mut args, "--mode");
                mode = match value.as_str() {
                    "recv" => ReceiveMode::Recv,
                    "inbox" => ReceiveMode::Inbox,
                    _ => panic!("invalid --mode value: {} (expected recv or inbox)", value),
                };
            }
            "--wo-crashes" => {
                wo_crashes = true;
            }
            "--wo-tags" => {
                use_tags = false;
            }
            _ => {
                panic!(
                    "unknown argument: {} (expected --participants <n>, --rounds <n>, --mode <recv|inbox>, --wo-crashes, --wo-tags)",
                    arg
                );
            }
        }
    }

    (num_participants, num_rounds, mode, wo_crashes, use_tags)
}

fn next_arg_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{} requires a value", flag))
}

fn main() {
    let (num_participants, num_rounds, mode, wo_crashes, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_participants, num_rounds, mode, !wo_crashes, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
