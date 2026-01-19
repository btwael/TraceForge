use std::any::type_name;
use std::cell::RefCell;
use std::collections::HashMap;

use crate::coverage::ExecutionId;
use crate::msg::Message;
use crate::runtime::execution::ExecutionState;
use crate::thread::ThreadId;
use crate::Val;

thread_local! {
    static ROUND_STATE: RefCell<RoundState> = RefCell::new(RoundState::default());
}

#[derive(Debug, Default)]
struct RoundState {
    execution_id: Option<ExecutionId>,
    rounds: HashMap<ThreadId, u32>,
}

fn with_round_state<F, R>(f: F) -> R
where
    F: FnOnce(&mut RoundState, ThreadId) -> R,
{
    let (tid, eid) = ExecutionState::with(|state| {
        let must = state.must.borrow();
        let tid = must.to_thread_id(state.current().id());
        let eid = must.telemetry.coverage.current_eid();
        (tid, eid)
    });

    ROUND_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.execution_id != Some(eid) {
            state.execution_id = Some(eid);
            state.rounds.clear();
        }
        f(&mut state, tid)
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rounds {
    _private: (),
}

impl Rounds {
    pub fn new() -> Self {
        Self { _private: () }
    }

    pub fn current(&self) -> Round {
        current_round()
    }

    pub fn advance(&mut self) -> Round {
        advance_round()
    }
}

fn current_round() -> Round {
    with_round_state(|state, tid| {
        let current = *state.rounds.entry(tid).or_insert(0);
        Round::new(current, tid)
    })
}

fn advance_round() -> Round {
    with_round_state(|state, tid| {
        let entry = state.rounds.entry(tid).or_insert(0);
        *entry = entry.checked_add(1).expect("round counter overflow");
        Round::new(*entry, tid)
    })
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RoundId(u32);

impl std::fmt::Display for RoundId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub struct Round {
    id: RoundId,
    thread: ThreadId,
}

impl Round {
    fn new(id: u32, thread: ThreadId) -> Self {
        Self {
            id: RoundId(id),
            thread,
        }
    }

    pub fn id(&self) -> RoundId {
        self.id
    }

    fn tag(&self) -> u32 {
        self.id.0
    }

    fn assert_current(&self) {
        let (current_tid, current_round) = with_round_state(|state, tid| {
            let current = *state.rounds.entry(tid).or_insert(0);
            (tid, current)
        });
        assert!(
            current_tid == self.thread,
            "round token belongs to thread {} but was used on thread {}",
            self.thread,
            current_tid
        );
        assert!(
            self.id.0 == current_round,
            "round token {} is not the current round {}",
            self.id,
            current_round
        );
    }
}

pub struct RoundMsg<T> {
    payload: T,
    round: RoundId,
}

impl<T> RoundMsg<T> {
    fn new(payload: T, round: RoundId) -> Self {
        Self { payload, round }
    }

    pub fn round_id(&self) -> RoundId {
        self.round
    }

    pub fn payload(&self, round: &Round) -> &T {
        self.assert_round(round);
        &self.payload
    }

    pub fn with_payload<R>(&self, round: &Round, f: impl FnOnce(&T) -> R) -> R {
        self.assert_round(round);
        f(&self.payload)
    }

    fn assert_round(&self, round: &Round) {
        round.assert_current();
        assert!(
            self.round >= round.id(),
            "message from round {} is not valid in round {}",
            self.round,
            round.id()
        );
    }
}

pub fn send<T: Message + 'static>(tid: ThreadId, msg: T, round: &Round) {
    round.assert_current();
    let tagged = TaggedVal::new(round.id(), Val::new(msg));
    crate::send_tagged_msg(tid, round.tag(), tagged);
}

pub fn send_lossy<T: Message + 'static>(tid: ThreadId, msg: T, round: &Round) {
    round.assert_current();
    let tagged = TaggedVal::new(round.id(), Val::new(msg));
    crate::send_tagged_lossy_msg(tid, round.tag(), tagged);
}

pub fn recv<T: Message + 'static>(round: &Round) -> Option<RoundMsg<T>> {
    round.assert_current();
    let min_tag = round.tag();
    let tagged: Option<TaggedVal> =
        crate::recv_tagged_msg(move |_tid, tag| tag.map_or(false, |t| t >= min_tag));
    tagged.map(|tagged| RoundMsg::new(expect_payload::<T>(tagged.payload), tagged.round))
}

pub fn recv_block<T: Message + 'static>(round: &Round) -> RoundMsg<T> {
    round.assert_current();
    let min_tag = round.tag();
    let tagged: TaggedVal =
        crate::recv_tagged_msg_block(move |_tid, tag| tag.map_or(false, |t| t >= min_tag));
    let payload = expect_payload::<T>(tagged.payload);
    RoundMsg::new(payload, tagged.round)
}

pub fn inbox(round: &Round) -> Vec<Option<RoundMsg<Val>>> {
    round.assert_current();
    let min_tag = round.tag();
    crate::inbox_with_tag_and_bounds(
        move |_tid, tag| tag.map_or(false, |t| t >= min_tag),
        0,
        None,
    )
    .into_iter()
    .map(|val| {
        val.map(|val| {
            let tagged = expect_tagged_val(val);
            RoundMsg::new(tagged.payload, tagged.round)
        })
    })
    .collect()
}

pub fn inbox_with_bounds(
    round: &Round,
    min: usize,
    max: Option<usize>,
) -> Vec<Option<RoundMsg<Val>>> {
    round.assert_current();
    let min_tag = round.tag();
    crate::inbox_with_tag_and_bounds(
        move |_tid, tag| tag.map_or(false, |t| t >= min_tag),
        min,
        max,
    )
    .into_iter()
    .map(|val| {
        val.map(|val| {
            let tagged = expect_tagged_val(val);
            RoundMsg::new(tagged.payload, tagged.round)
        })
    })
    .collect()
}

#[derive(Clone, Debug, PartialEq)]
struct TaggedVal {
    round: RoundId,
    payload: Val,
}

impl TaggedVal {
    fn new(round: RoundId, payload: Val) -> Self {
        Self { round, payload }
    }
}

fn expect_tagged_val(val: Val) -> TaggedVal {
    match val.as_any().downcast::<TaggedVal>() {
        Ok(v) => *v,
        Err(_) => {
            panic!(
                "wrong message return type; expecting {} but got {}",
                type_name::<TaggedVal>(),
                val.type_name
            );
        }
    }
}

fn expect_payload<T: 'static>(val: Val) -> T {
    match val.as_any().downcast::<T>() {
        Ok(v) => *v,
        Err(_) => {
            panic!(
                "wrong message return type; expecting {} but got {}",
                type_name::<T>(),
                val.type_name
            );
        }
    }
}
