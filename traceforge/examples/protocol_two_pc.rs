use traceforge::comm_close::Rounds;
use traceforge::thread;
use traceforge::thread::ThreadId;

const DEFAULT_NUM_PARTICIPANTS: usize = 3;
const DEFAULT_NUM_ROUNDS: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Inbox;
const DEFAULT_USE_TAGS: bool = true;
const INIT_TAG: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, traceforge::DimensionEnum)]
enum Phase {
    Prepare,
    Commit,
}

#[derive(traceforge::Round)]
struct TwoPcRound {
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
    rounds: Rounds<TwoPcRound>,
    num_rounds: u32,
    mode: ReceiveMode,
}

impl Node {
    fn new(init: InitMsg, role: Role, num_rounds: u32, mode: ReceiveMode, use_tags: bool) -> Self {
        let me = thread::current().id();
        let rounds = if use_tags {
            Rounds::<TwoPcRound>::new()
        } else {
            Rounds::<TwoPcRound>::new_wo_tags(false)
        };
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
        let me = self.me;
        for _ in 0..self.num_rounds {
            let prepare = PrepareMsg { sender: me };
            self.broadcast(Message::Prepare(prepare));

            let votes = collect_votes(&self.rounds, self.participants.len(), self.mode);
            let all_yes = votes.iter().all(|msg| msg.vote == Vote::Yes);
            let decision = if all_yes {
                Decision::Commit
            } else {
                Decision::Abort
            };

            self.rounds.advance(TwoPcRound::phase());

            let decide = DecideMsg {
                decision,
                sender: me,
            };
            self.broadcast(Message::Decide(decide));

            self.rounds.advance(TwoPcRound::round());
        }
    }

    fn run_participant(&mut self) {
        let coordinator = self.coordinator;
        let me = self.me;
        for _ in 0..self.num_rounds {
            let _prepare = collect_prepare(&self.rounds);

            let vote_yes = traceforge::nondet();
            let vote = if vote_yes { Vote::Yes } else { Vote::No };
            self.rounds
                .send(coordinator, Message::Vote(VoteMsg { vote, sender: me }));

            self.rounds.advance(TwoPcRound::phase());

            let decision = collect_decide(&self.rounds);
            match decision.decision {
                Decision::Commit => assert!(vote_yes),
                Decision::Abort => (),
            }

            self.rounds.advance(TwoPcRound::round());
        }
    }

    fn broadcast(&self, msg: Message) {
        for node in self.participants.iter() {
            self.rounds.send(*node, msg.clone());
        }
    }
}

fn collect_prepare(rounds: &Rounds<TwoPcRound>) -> PrepareMsg {
    let msg = rounds.recv_block::<Message>();
    match msg.payload() {
        Message::Prepare(payload) => payload.clone(),
        _ => panic!("expected PrepareMsg"),
    }
}

fn collect_votes_inbox(rounds: &Rounds<TwoPcRound>, expected: usize) -> Vec<VoteMsg> {
    let msgs = rounds.inbox_with_bounds::<Message>(expected, Some(expected));
    let mut out = Vec::new();
    for msg in msgs.into_iter().flatten() {
        out.push(expect_vote(msg.payload()));
    }
    if out.len() != expected {
        panic!("expected {} messages, got {}", expected, out.len());
    }
    out
}

fn collect_votes_recv(rounds: &Rounds<TwoPcRound>, expected: usize) -> Vec<VoteMsg> {
    let mut out = Vec::new();
    while out.len() < expected {
        let msg = rounds.recv_block::<Message>();
        out.push(expect_vote(msg.payload()));
    }
    out
}

fn collect_votes(rounds: &Rounds<TwoPcRound>, expected: usize, mode: ReceiveMode) -> Vec<VoteMsg> {
    match mode {
        ReceiveMode::Recv => collect_votes_recv(rounds, expected),
        ReceiveMode::Inbox => collect_votes_inbox(rounds, expected),
    }
}

fn collect_decide(rounds: &Rounds<TwoPcRound>) -> DecideMsg {
    let msg = rounds.recv_block::<Message>();
    match msg.payload() {
        Message::Decide(payload) => payload.clone(),
        _ => panic!("expected DecideMsg"),
    }
}

fn expect_vote(msg: &Message) -> VoteMsg {
    match msg {
        Message::Vote(payload) => payload.clone(),
        _ => panic!("expected VoteMsg"),
    }
}

fn start_node(role: Role, num_rounds: u32, mode: ReceiveMode, use_tags: bool) {
    let init: Message = if use_tags {
        traceforge::recv_tagged_msg_block(|_, tag| tag.is_none())
    } else {
        traceforge::recv_tagged_msg_block(|_, tag| tag == Some(INIT_TAG))
    };
    let init = match init {
        Message::Init(init) => init,
        _ => panic!("expected init message"),
    };
    Node::new(init, role, num_rounds, mode, use_tags).run();
}

fn run_protocol(
    num_participants: usize,
    num_rounds: u32,
    mode: ReceiveMode,
    use_tags: bool,
) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let mut handles = Vec::new();

        let coordinator_handle = thread::spawn(move || {
            start_node(Role::Coordinator, num_rounds, mode, use_tags)
        });
        let coordinator = coordinator_handle.thread().id();
        handles.push(coordinator_handle);

        let mut participant_ids = Vec::new();
        for _ in 0..num_participants {
            let handle = thread::spawn(move || {
                start_node(Role::Participant, num_rounds, mode, use_tags)
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

fn parse_args() -> (usize, u32, ReceiveMode, bool) {
    let mut num_participants = DEFAULT_NUM_PARTICIPANTS;
    let mut num_rounds = DEFAULT_NUM_ROUNDS;
    let mut mode = DEFAULT_MODE;
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
            "--wo-tags" => {
                use_tags = false;
            }
            _ => {
                panic!(
                    "unknown argument: {} (expected only --participants <n>, --rounds <n>, --mode <recv|inbox>, --wo-tags)",
                    arg
                );
            }
        }
    }

    (num_participants, num_rounds, mode, use_tags)
}

fn next_arg_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{} requires a value", flag))
}

fn main() {
    let (num_participants, num_rounds, mode, use_tags) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode recv");
    }
    let stats = run_protocol(num_participants, num_rounds, mode, use_tags);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
