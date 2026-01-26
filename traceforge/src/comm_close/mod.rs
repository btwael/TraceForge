use std::any::type_name;
use std::iter;
use std::sync::Arc;

use crate::channel::{self_loc_comm, thread_loc_comm};
use crate::msg::Message;
use crate::predicate::PredicateType;
use crate::thread::ThreadId;
use crate::Val;

pub mod round;
pub use round::{
    LevelSpec, Round, RoundFilter, RoundId, RoundScheme, RoundSchemeBuilder, RoundStamp, Rounds,
    TagCmp,
};

fn ensure_len(label: &str, expected: usize, actual: usize) {
    if expected != actual {
        panic!(
            "{} length {} does not match scheme levels {}",
            label, actual, expected
        );
    }
}

fn matches_components_with_cmp(
    scheme: &RoundScheme,
    tag: &[u32],
    round: &[u32],
    cmp_overrides: &[Option<TagCmp>],
) -> bool {
    let expected = scheme.level_count();
    ensure_len("tag", expected, tag.len());
    ensure_len("round", expected, round.len());
    if !cmp_overrides.is_empty() {
        ensure_len("comparison overrides", expected, cmp_overrides.len());
    }
    for (index, level) in scheme.levels().iter().enumerate() {
        let cmp = if cmp_overrides.is_empty() {
            level.default_cmp
        } else {
            cmp_overrides[index].unwrap_or(level.default_cmp)
        };
        let ok = match cmp {
            TagCmp::Eq => tag[index] == round[index],
            TagCmp::Gte => tag[index] >= round[index],
        };
        if !ok {
            return false;
        }
    }
    true
}

fn matches_components(scheme: &RoundScheme, tag: &[u32], round: &[u32]) -> bool {
    matches_components_with_cmp(scheme, tag, round, &[])
}

fn round_tag_predicate(round: &Round) -> PredicateType {
    let scheme = round.scheme().clone();
    let round_components = round.components().to_vec();
    ensure_len("round", scheme.level_count(), round_components.len());
    PredicateType(Arc::new(move |_tid, tag| {
        let tag_vec = match tag {
            Some(tag_vec) => tag_vec,
            None => return false,
        };
        matches_components(&scheme, &tag_vec, &round_components)
    }))
}

fn filter_tag_predicate(filter: &RoundFilter) -> PredicateType {
    let scheme = filter.scheme().clone();
    let round_components = filter.components().to_vec();
    let cmp_overrides = filter.cmp_overrides().to_vec();
    ensure_len("round", scheme.level_count(), round_components.len());
    ensure_len(
        "comparison overrides",
        scheme.level_count(),
        cmp_overrides.len(),
    );
    PredicateType(Arc::new(move |_tid, tag| {
        let tag_vec = match tag {
            Some(tag_vec) => tag_vec,
            None => return false,
        };
        matches_components_with_cmp(&scheme, &tag_vec, &round_components, &cmp_overrides)
    }))
}

impl Rounds {
    pub fn advance(&mut self) -> Round {
        self.advance_round()
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
        self.round.clone()
    }

    pub fn round_stamp(&self) -> RoundStamp {
        RoundStamp::from(&self.round)
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
            matches_components(round.scheme(), self.round.components(), round.components()),
            "message from round {:?} is not valid in round {:?}",
            self.round,
            round.id()
        );
    }
}

pub fn send<T: Message + 'static>(tid: ThreadId, msg: T, round: &Round) {
    round.assert_current();
    let tagged = TaggedVal::new(round.id().clone(), Val::new(msg));
    let tag = round.components().to_vec();
    let (loc, comm) = thread_loc_comm(tid);
    crate::send_msg_with_tag_vec(tagged, Some(tag), &loc, comm, false);
}

pub fn send_lossy<T: Message + 'static>(tid: ThreadId, msg: T, round: &Round) {
    round.assert_current();
    let tagged = TaggedVal::new(round.id().clone(), Val::new(msg));
    let tag = round.components().to_vec();
    let (loc, comm) = thread_loc_comm(tid);
    crate::send_msg_with_tag_vec(tagged, Some(tag), &loc, comm, true);
}

pub fn recv<T: Message + 'static>(round: &Round) -> Option<RoundMsg<T>> {
    round.assert_current();
    let tag_predicate = round_tag_predicate(round);
    let (loc, comm) = self_loc_comm();
    let tagged: Option<TaggedVal> =
        crate::recv_msg_with_tag(iter::once(&loc), comm, Some(tag_predicate)).map(|x| x.0);
    tagged.map(|tagged| RoundMsg::new(expect_payload::<T>(tagged.payload), tagged.round))
}

pub fn recv_block<T: Message + 'static>(round: &Round) -> RoundMsg<T> {
    round.assert_current();
    let tag_predicate = round_tag_predicate(round);
    let (loc, comm) = self_loc_comm();
    let tagged: TaggedVal =
        crate::recv_msg_block_with_tag(iter::once(&loc), comm, Some(tag_predicate)).0;
    let payload = expect_payload::<T>(tagged.payload);
    RoundMsg::new(payload, tagged.round)
}

pub fn recv_with_filter<T: Message + 'static>(filter: &RoundFilter) -> Option<RoundMsg<T>> {
    filter.assert_current();
    let tag_predicate = filter_tag_predicate(filter);
    let (loc, comm) = self_loc_comm();
    let tagged: Option<TaggedVal> =
        crate::recv_msg_with_tag(iter::once(&loc), comm, Some(tag_predicate)).map(|x| x.0);
    tagged.map(|tagged| RoundMsg::new(expect_payload::<T>(tagged.payload), tagged.round))
}

pub fn recv_block_with_filter<T: Message + 'static>(filter: &RoundFilter) -> RoundMsg<T> {
    filter.assert_current();
    let tag_predicate = filter_tag_predicate(filter);
    let (loc, comm) = self_loc_comm();
    let tagged: TaggedVal =
        crate::recv_msg_block_with_tag(iter::once(&loc), comm, Some(tag_predicate)).0;
    let payload = expect_payload::<T>(tagged.payload);
    RoundMsg::new(payload, tagged.round)
}

pub fn inbox(round: &Round) -> Vec<Option<RoundMsg<Val>>> {
    inbox_with_bounds(round, 0, None)
}

pub fn inbox_with_bounds(
    round: &Round,
    min: usize,
    max: Option<usize>,
) -> Vec<Option<RoundMsg<Val>>> {
    round.assert_current();
    let tag_predicate = round_tag_predicate(round);
    crate::inbox_extended(Some(tag_predicate), min, max)
        .into_iter()
        .map(|val| {
            val.map(|val| {
                let tagged = expect_tagged_val(val);
                RoundMsg::new(tagged.payload, tagged.round)
            })
        })
        .collect()
}

pub fn inbox_with_filter(filter: &RoundFilter) -> Vec<Option<RoundMsg<Val>>> {
    inbox_with_bounds_filter(filter, 0, None)
}

pub fn inbox_with_bounds_filter(
    filter: &RoundFilter,
    min: usize,
    max: Option<usize>,
) -> Vec<Option<RoundMsg<Val>>> {
    filter.assert_current();
    let tag_predicate = filter_tag_predicate(filter);
    crate::inbox_extended(Some(tag_predicate), min, max)
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
