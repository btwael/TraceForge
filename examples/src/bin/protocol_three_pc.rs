use traceforge::comm_close::{self, TraceForgeTransportMode};
use traceforge::thread;
use traceforge::thread::ThreadId;
use traceforge::{BranchingStrategy, Nondet};
use traceforge_rounds::{Comm, Dim, Round};

const DEFAULT_NUM_PARTICIPANTS: usize = 3;
const DEFAULT_NUM_ROUNDS: u32 = 1;
const DEFAULT_MODE: ReceiveMode = ReceiveMode::Inbox;
const DEFAULT_USE_TAGS: bool = true;
const INIT_TAG: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Dim)]
enum Phase {
    Prepare,
    PreCommit,
    Commit,
}

#[derive(Clone, Debug, Eq, PartialEq, Round)]
struct ThreePcRound {
    round: u32,
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
    comm: Comm<ThreePcRound, comm_close::TraceForgeTransport>,
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
        let me = thread::current_id();
        let comm = comm_close::comm_with::<ThreePcRound>(transport_mode(mode, use_tags));
        Self {
            role,
            participants: init.participants,
            coordinator: init.coordinator,
            me,
            comm,
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
            self.broadcast(Message::Prepare(PrepareMsg { sender: me }));

            let votes = collect_votes(&mut self.comm, self.participants.len(), self.mode);
            let all_yes = votes.len() == self.participants.len()
                && votes.iter().all(|msg| msg.vote == Vote::Yes);

            if !all_yes {
                self.comm.rounds().advance(ThreePcRound::dim_phase());
                self.broadcast(Message::Decide(DecideMsg {
                    decision: Decision::Abort,
                    sender: me,
                }));
                self.comm.rounds().advance(ThreePcRound::dim_round());
                continue;
            }

            self.comm.rounds().advance(ThreePcRound::dim_phase());
            self.broadcast(Message::PreCommit(PreCommitMsg {
                coordinator: me,
                sender: me,
            }));

            if self.crashes_enabled && traceforge::nondet() {
                return;
            }

            let acks = collect_acks(&mut self.comm, self.participants.len(), self.mode);
            if acks.len() != self.participants.len() {
                self.comm.rounds().advance(ThreePcRound::dim_round());
                continue;
            }

            self.comm.rounds().advance(ThreePcRound::dim_phase());
            self.broadcast(Message::Decide(DecideMsg {
                decision: Decision::Commit,
                sender: me,
            }));
            self.comm.rounds().advance(ThreePcRound::dim_round());
        }
    }

    fn run_participant(&mut self) {
        let coordinator = self.coordinator;
        let me = self.me;
        for _ in 0..self.num_rounds {
            let _prepare = collect_prepare(&mut self.comm);

            if self.crashes_enabled && traceforge::nondet() {
                return;
            }

            let vote_yes = traceforge::nondet();
            let vote = if vote_yes { Vote::Yes } else { Vote::No };
            self.comm
                .send(coordinator, Message::Vote(VoteMsg { vote, sender: me }))
                .unwrap();

            self.comm.rounds().advance(ThreePcRound::dim_phase());
            let msg = collect_precommit_or_abort(&mut self.comm);
            let Some(msg) = msg else {
                return;
            };

            match msg {
                Message::Decide(decide) => {
                    assert_eq!(decide.decision, Decision::Abort);
                    self.comm.rounds().advance(ThreePcRound::dim_round());
                    continue;
                }
                Message::PreCommit(_) => {
                    if !vote_yes {
                        panic!("received PreCommit after voting No");
                    }
                }
                _ => panic!("expected PreCommit or Decide(Abort)"),
            }

            self.comm
                .send(coordinator, Message::Ack(AckMsg { sender: me }))
                .unwrap();

            self.comm.rounds().advance(ThreePcRound::dim_phase());
            let decision = collect_optional_decide(&mut self.comm, self.mode);
            let decided = decision.is_some();
            if let Some(decide) = decision {
                assert_eq!(decide.decision, Decision::Commit);
            }

            self.comm.rounds().advance(ThreePcRound::dim_round());

            if !decided {
                return;
            }
        }
    }

    fn broadcast(&mut self, msg: Message) {
        for node in self.participants.iter() {
            self.comm.send(*node, msg.clone()).unwrap();
        }
    }
}

fn collect_prepare(comm: &mut Comm<ThreePcRound, comm_close::TraceForgeTransport>) -> PrepareMsg {
    let msg = comm
        .recv_block_with::<Message, _>(same_round_filter)
        .unwrap();
    match msg {
        Message::Prepare(payload) => payload,
        _ => panic!("expected PrepareMsg"),
    }
}

fn collect_votes(
    comm: &mut Comm<ThreePcRound, comm_close::TraceForgeTransport>,
    expected: usize,
    mode: ReceiveMode,
) -> Vec<VoteMsg> {
    let messages = match mode {
        ReceiveMode::Recv => collect_bounded_recv(comm, 0, expected),
        ReceiveMode::Inbox => collect_bounded_inbox(comm, 0, expected),
    };
    messages.into_iter().map(expect_vote).collect()
}

fn collect_acks(
    comm: &mut Comm<ThreePcRound, comm_close::TraceForgeTransport>,
    expected: usize,
    mode: ReceiveMode,
) -> Vec<AckMsg> {
    match mode {
        ReceiveMode::Recv => {
            let count = if expected == 0 {
                0
            } else {
                (0..=expected).nondet()
            };
            let mut out = Vec::new();
            for _ in 0..count {
                let msg = comm
                    .recv_block_with::<Message, _>(same_round_filter)
                    .unwrap();
                out.push(expect_ack(msg));
            }
            out
        }
        ReceiveMode::Inbox => {
            let messages = collect_bounded_inbox(comm, 0, expected);
            messages.into_iter().map(expect_ack).collect()
        }
    }
}

fn collect_precommit_or_abort(
    comm: &mut Comm<ThreePcRound, comm_close::TraceForgeTransport>,
) -> Option<Message> {
    let payload = comm.recv_with::<Message, _>(same_round_filter).unwrap()?;
    match &payload {
        Message::PreCommit(_) => Some(payload),
        Message::Decide(decide) => {
            assert_eq!(decide.decision, Decision::Abort);
            Some(payload)
        }
        _ => panic!("expected PreCommit or Decide(Abort)"),
    }
}

fn collect_optional_decide(
    comm: &mut Comm<ThreePcRound, comm_close::TraceForgeTransport>,
    mode: ReceiveMode,
) -> Option<DecideMsg> {
    match mode {
        ReceiveMode::Recv => {
            let msg = comm.recv_with::<Message, _>(same_round_filter).unwrap()?;
            match msg {
                Message::Decide(payload) => Some(payload),
                _ => panic!("expected DecideMsg"),
            }
        }
        ReceiveMode::Inbox => {
            let msgs = comm
                .inbox_with_bounds_with::<Message, _>(0, Some(1), same_round_filter)
                .unwrap();
            for msg in msgs.into_iter().flatten() {
                match msg {
                    Message::Decide(decide) => return Some(decide),
                    _ => panic!("expected DecideMsg"),
                }
            }
            None
        }
    }
}

fn collect_bounded_recv(
    comm: &mut Comm<ThreePcRound, comm_close::TraceForgeTransport>,
    min: usize,
    max: usize,
) -> Vec<Message> {
    assert!(max >= min, "requires max >= min");
    let count = if max == min {
        min
    } else {
        (min..=max).nondet()
    };
    let mut out = Vec::new();
    for _ in 0..count {
        let Some(msg) = comm.recv_with::<Message, _>(same_round_filter).unwrap() else {
            break;
        };
        out.push(msg);
    }
    out
}

fn collect_bounded_inbox(
    comm: &mut Comm<ThreePcRound, comm_close::TraceForgeTransport>,
    min: usize,
    max: usize,
) -> Vec<Message> {
    assert!(max >= min, "requires max >= min");
    let msgs = comm
        .inbox_with_bounds_with::<Message, _>(min, Some(max), same_round_filter)
        .unwrap();
    let mut out = Vec::new();
    for msg in msgs.into_iter().flatten() {
        out.push(msg);
    }
    out
}

fn same_round_filter(local: &ThreePcRound, remote: &ThreePcRound) -> bool {
    local.round() == remote.round() && local.phase() == remote.phase()
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
    parallel: ParallelMode,
) -> traceforge::Stats {
    let mut config = traceforge::Config::builder();
    match parallel {
        ParallelMode::Sequential => {}
        ParallelMode::Shared(workers) => {
            config = config.with_parallel(true).with_parallel_workers(workers);
        }
        ParallelMode::Rayon(workers) => {
            config = config
                .with_partitioned_parallelization(true)
                .with_partitioned_num_threads(workers)
                .with_partitioned_branching(BranchingStrategy::RevisitQueueRayon)
                .with_iterations_until_split(5000);
        }
    }

    traceforge::verify(config.build(), move || {
        let mut handles = Vec::new();

        let coordinator_handle = thread::spawn(move || {
            start_node(
                Role::Coordinator,
                num_rounds,
                mode,
                crashes_enabled,
                use_tags,
            )
        });
        let coordinator = coordinator_handle.thread().id();
        handles.push(coordinator_handle);

        let mut participant_ids = Vec::new();
        for _ in 0..num_participants {
            let handle = thread::spawn(move || {
                start_node(
                    Role::Participant,
                    num_rounds,
                    mode,
                    crashes_enabled,
                    use_tags,
                )
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
                traceforge::send_tagged_msg(
                    handle.thread().id(),
                    INIT_TAG,
                    Message::Init(init.clone()),
                );
            }
        }

        for handle in handles {
            handle.join().unwrap();
        }
    })
}

fn transport_mode(mode: ReceiveMode, use_tags: bool) -> TraceForgeTransportMode {
    match (mode, use_tags) {
        (ReceiveMode::Inbox, true) => TraceForgeTransportMode::TaggedNativeInbox,
        (ReceiveMode::Recv, true) => TraceForgeTransportMode::TaggedRepeatedRecv,
        (ReceiveMode::Recv, false) => TraceForgeTransportMode::UntaggedRepeatedRecv,
        (ReceiveMode::Inbox, false) => {
            panic!("--wo-tags is only supported with --mode rounds/recv");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParallelMode {
    Sequential,
    Shared(usize),
    Rayon(usize),
}

fn parse_args() -> (usize, u32, ReceiveMode, bool, bool, ParallelMode) {
    let mut num_participants = DEFAULT_NUM_PARTICIPANTS;
    let mut num_rounds = DEFAULT_NUM_ROUNDS;
    let mut mode = DEFAULT_MODE;
    let mut wo_crashes = false;
    let mut use_tags = DEFAULT_USE_TAGS;
    let mut explicit_wo_tags = false;
    let mut parallel = ParallelMode::Sequential;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--participants" => {
                let value = next_arg_value(&mut args, "--participants");
                num_participants = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --participants value: {value}"));
            }
            "--rounds" => {
                let value = next_arg_value(&mut args, "--rounds");
                num_rounds = value
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid --rounds value: {value}"));
            }
            "--mode" => {
                let value = next_arg_value(&mut args, "--mode");
                let parsed = parse_mode(&value);
                mode = parsed.0;
                use_tags = parsed.1 && !explicit_wo_tags;
            }
            "--wo-crashes" => {
                wo_crashes = true;
            }
            "--wo-tags" => {
                explicit_wo_tags = true;
                use_tags = false;
            }
            "--parallel" => {
                let workers = parse_workers(&mut args, "--parallel");
                if parallel != ParallelMode::Sequential {
                    panic!("only one parallel mode can be selected");
                }
                parallel = ParallelMode::Shared(workers);
            }
            "--rayon" => {
                let workers = parse_workers(&mut args, "--rayon");
                if parallel != ParallelMode::Sequential {
                    panic!("only one parallel mode can be selected");
                }
                parallel = ParallelMode::Rayon(workers);
            }
            _ => {
                panic!(
                    "unknown argument: {arg} (expected --participants <n>, --rounds <n>, --mode <full|rounds|dpor>, --wo-crashes, --wo-tags, --parallel <n>, --rayon <n>)",
                );
            }
        }
    }

    (
        num_participants,
        num_rounds,
        mode,
        wo_crashes,
        use_tags,
        parallel,
    )
}

fn parse_workers(args: &mut impl Iterator<Item = String>, flag: &str) -> usize {
    let value = next_arg_value(args, flag);
    let workers = value
        .parse()
        .unwrap_or_else(|_| panic!("invalid {flag} value: {value}"));
    if workers == 0 {
        panic!("{flag} requires a positive worker count");
    }
    workers
}

fn next_arg_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
}

fn parse_mode(value: &str) -> (ReceiveMode, bool) {
    match value {
        "full" | "inbox" => (ReceiveMode::Inbox, true),
        "rounds" | "recv" => (ReceiveMode::Recv, true),
        "dpor" => (ReceiveMode::Recv, false),
        _ => panic!("invalid --mode value: {value} (expected full, rounds, dpor, inbox, or recv)"),
    }
}

fn main() {
    let (num_participants, num_rounds, mode, wo_crashes, use_tags, parallel) = parse_args();
    if !use_tags && mode == ReceiveMode::Inbox {
        panic!("--wo-tags is only supported with --mode rounds/recv");
    }
    let stats = run_protocol(
        num_participants,
        num_rounds,
        mode,
        !wo_crashes,
        use_tags,
        parallel,
    );
    println!("Stats = {}, {}", stats.execs, stats.block);
}
