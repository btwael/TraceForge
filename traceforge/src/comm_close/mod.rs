pub mod round;

pub use round::{
    EnumCodec, EnumLevelBuilder, LevelKind, LevelSpec, Round, RoundEnum, RoundKey, RoundScheme,
    RoundSchemeBuilder, RoundStamp, RoundStampView, RoundOrder, Rounds, DefaultMatch,
    DefaultFilter, U32LevelBuilder,
};

use crate::channel::{self_loc_comm, thread_loc_comm};
use crate::msg::{Message, Val};
use crate::predicate::PredicateType;
use crate::thread::ThreadId;
use std::any::type_name;
use std::iter;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct RoundMsg<T> {
    payload: T,
    stamp: RoundStamp,
}

impl<T> RoundMsg<T> {
    fn new(payload: T, stamp: RoundStamp) -> Self {
        Self { payload, stamp }
    }

    pub fn payload(&self, round: &Round) -> &T {
        self.assert_round(round);
        &self.payload
    }

    pub fn with_payload<R>(&self, round: &Round, f: impl FnOnce(&T) -> R) -> R {
        self.assert_round(round);
        f(&self.payload)
    }

    pub fn stamp(&self) -> &RoundStamp {
        &self.stamp
    }

    pub fn round_stamp(&self) -> RoundStamp {
        self.stamp.clone()
    }

    fn assert_round(&self, round: &Round) {
        round.assert_current();
        let tag = self.stamp.components();
        let current = round.components();
        ensure_len("message round", current.len(), tag.len());
        assert!(
            matches_components_lexicographic(tag, current),
            "message from round {:?} is not valid in round {:?}",
            self.stamp,
            round.components()
        );
    }
}

#[derive(Clone)]
pub struct RoundFilter {
    scheme: Arc<RoundScheme>,
    components: Vec<u32>,
    base_matches: Vec<DefaultMatch>,
    cmp_overrides: Vec<Option<DefaultMatch>>,
    order: RoundOrder,
}

impl RoundFilter {
    fn new(scheme: Arc<RoundScheme>, components: Vec<u32>) -> Self {
        let level_count = scheme.levels().len();
        let (order, base_matches) = match scheme.default_filter() {
            Some(filter) => (filter.order(), filter.matches().to_vec()),
            None => (
                scheme.default_round_order(),
                vec![DefaultMatch::Any; level_count],
            ),
        };
        Self {
            scheme,
            components,
            base_matches,
            cmp_overrides: vec![None; level_count],
            order,
        }
    }

    pub fn level_cmp<K: RoundKey>(mut self, key: K, cmp: DefaultMatch) -> Self {
        let index = self.scheme.key_position(key);
        self.cmp_overrides[index] = Some(cmp);
        self
    }

    pub fn round_order(mut self, order: RoundOrder) -> Self {
        self.order = order;
        self
    }

    fn assert_current(&self) {
        let current = round::current_round_stamp_for_send();
        assert!(
            self.components == current.components(),
            "round filter {:?} is not the current round {:?}",
            self.components,
            current.components()
        );
    }

    fn scheme(&self) -> &RoundScheme {
        &self.scheme
    }

    fn components(&self) -> &[u32] {
        &self.components
    }

    fn base_matches(&self) -> &[DefaultMatch] {
        &self.base_matches
    }

    fn cmp_overrides(&self) -> &[Option<DefaultMatch>] {
        &self.cmp_overrides
    }

    fn order(&self) -> RoundOrder {
        self.order
    }
}

impl Round {
    pub fn filter(&self) -> RoundFilter {
        RoundFilter::new(Arc::new(self.scheme().clone()), self.components().to_vec())
    }
}

pub fn send<T: Message + 'static>(tid: ThreadId, msg: T) {
    let stamp = round::current_round_stamp_for_send();
    send_with_stamp(tid, msg, stamp, false);
}

pub fn send_lossy<T: Message + 'static>(tid: ThreadId, msg: T) {
    let stamp = round::current_round_stamp_for_send();
    send_with_stamp(tid, msg, stamp, true);
}

pub fn send_with_round<T: Message + 'static>(tid: ThreadId, msg: T, round: &Round) {
    round.assert_current();
    send_with_stamp(tid, msg, round.stamp(), false);
}

pub fn send_lossy_with_round<T: Message + 'static>(tid: ThreadId, msg: T, round: &Round) {
    round.assert_current();
    send_with_stamp(tid, msg, round.stamp(), true);
}

fn send_with_stamp<T: Message + 'static>(tid: ThreadId, msg: T, stamp: RoundStamp, lossy: bool) {
    let tag = stamp.components().to_vec();
    let tagged = TaggedVal::new(stamp, Val::new(msg));
    let (loc, comm) = thread_loc_comm(tid);
    crate::send_msg_with_tag_vec(tagged, Some(tag), &loc, comm, lossy);
}

pub fn recv<T: Message + 'static>(round: &Round) -> Option<RoundMsg<T>> {
    round.assert_current();
    let tag_predicate = round_tag_predicate(round);
    let (loc, comm) = self_loc_comm();
    let tagged: Option<TaggedVal> =
        crate::recv_msg_with_tag(iter::once(&loc), comm, Some(tag_predicate)).map(|x| x.0);
    tagged.map(|tagged| RoundMsg::new(expect_payload::<T>(tagged.payload), tagged.stamp))
}

pub fn recv_block<T: Message + 'static>(round: &Round) -> RoundMsg<T> {
    round.assert_current();
    let tag_predicate = round_tag_predicate(round);
    let (loc, comm) = self_loc_comm();
    let tagged: TaggedVal =
        crate::recv_msg_block_with_tag(iter::once(&loc), comm, Some(tag_predicate)).0;
    let payload = expect_payload::<T>(tagged.payload);
    RoundMsg::new(payload, tagged.stamp)
}

pub fn recv_with_filter<T: Message + 'static>(filter: &RoundFilter) -> Option<RoundMsg<T>> {
    filter.assert_current();
    let tag_predicate = filter_tag_predicate(filter);
    let (loc, comm) = self_loc_comm();
    let tagged: Option<TaggedVal> =
        crate::recv_msg_with_tag(iter::once(&loc), comm, Some(tag_predicate)).map(|x| x.0);
    tagged.map(|tagged| RoundMsg::new(expect_payload::<T>(tagged.payload), tagged.stamp))
}

pub fn recv_block_with_filter<T: Message + 'static>(filter: &RoundFilter) -> RoundMsg<T> {
    filter.assert_current();
    let tag_predicate = filter_tag_predicate(filter);
    let (loc, comm) = self_loc_comm();
    let tagged: TaggedVal =
        crate::recv_msg_block_with_tag(iter::once(&loc), comm, Some(tag_predicate)).0;
    let payload = expect_payload::<T>(tagged.payload);
    RoundMsg::new(payload, tagged.stamp)
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
                RoundMsg::new(tagged.payload, tagged.stamp)
            })
        })
        .collect()
}

fn round_tag_predicate(round: &Round) -> PredicateType {
    let levels = round.scheme().levels().to_vec();
    let round_components = round.components().to_vec();
    let (order, base_matches) = match round.scheme().default_filter() {
        Some(filter) => (filter.order(), filter.matches().to_vec()),
        None => (
            round.scheme().default_round_order(),
            vec![DefaultMatch::Any; levels.len()],
        ),
    };
    PredicateType(Arc::new(move |_tid, tag| {
        let tag_vec = match tag {
            Some(tag_vec) => tag_vec,
            None => return false,
        };
        matches_components(
            &levels,
            &tag_vec,
            &round_components,
            &base_matches,
            order,
        )
    }))
}

fn matches_components(
    levels: &[LevelSpec],
    tag: &[u32],
    round: &[u32],
    base_matches: &[DefaultMatch],
    order: RoundOrder,
) -> bool {
    let expected = levels.len();
    ensure_len("tag", expected, tag.len());
    ensure_len("round", expected, round.len());
    ensure_len("default matches", expected, base_matches.len());
    let order_ok = match order {
        RoundOrder::ComponentWise => {
            for (t, r) in tag.iter().zip(round.iter()) {
                if t < r {
                    return false;
                }
            }
            true
        }
        RoundOrder::Lexicographic => matches_components_lexicographic(tag, round),
    };
    if !order_ok {
        return false;
    }
    for (index, _) in levels.iter().enumerate() {
        let ok = match base_matches[index] {
            DefaultMatch::Eq => tag[index] == round[index],
            DefaultMatch::Gte => tag[index] >= round[index],
            DefaultMatch::Any => true,
        };
        if !ok {
            return false;
        }
    }
    true
}

fn filter_tag_predicate(filter: &RoundFilter) -> PredicateType {
    let levels = filter.scheme().levels().to_vec();
    let round_components = filter.components().to_vec();
    let base_matches = filter.base_matches().to_vec();
    let cmp_overrides = filter.cmp_overrides().to_vec();
    let order = filter.order();
    PredicateType(Arc::new(move |_tid, tag| {
        let tag_vec = match tag {
            Some(tag_vec) => tag_vec,
            None => return false,
        };
        matches_components_with_cmp(
            &levels,
            &tag_vec,
            &round_components,
            &base_matches,
            &cmp_overrides,
            order,
        )
    }))
}

fn matches_components_with_cmp(
    levels: &[LevelSpec],
    tag: &[u32],
    round: &[u32],
    base_matches: &[DefaultMatch],
    cmp_overrides: &[Option<DefaultMatch>],
    order: RoundOrder,
) -> bool {
    let expected = levels.len();
    ensure_len("tag", expected, tag.len());
    ensure_len("round", expected, round.len());
    ensure_len("default matches", expected, base_matches.len());
    ensure_len("comparison overrides", expected, cmp_overrides.len());
    let order_ok = match order {
        RoundOrder::ComponentWise => {
            for (t, r) in tag.iter().zip(round.iter()) {
                if t < r {
                    return false;
                }
            }
            true
        }
        RoundOrder::Lexicographic => matches_components_lexicographic(tag, round),
    };
    if !order_ok {
        return false;
    }
    for (index, _) in levels.iter().enumerate() {
        let cmp = cmp_overrides[index].unwrap_or(base_matches[index]);
        let ok = match cmp {
            DefaultMatch::Eq => tag[index] == round[index],
            DefaultMatch::Gte => tag[index] >= round[index],
            DefaultMatch::Any => true,
        };
        if !ok {
            return false;
        }
    }
    true
}

fn matches_components_lexicographic(tag: &[u32], round: &[u32]) -> bool {
    ensure_len("tag", round.len(), tag.len());
    ensure_len("round", tag.len(), round.len());
    for (t, r) in tag.iter().zip(round.iter()) {
        if t > r {
            return true;
        } else if t < r {
            return false;
        }
    }
    true
}

fn ensure_len(label: &str, expected: usize, actual: usize) {
    if expected != actual {
        panic!(
            "{} length {} does not match scheme levels {}",
            label, actual, expected
        );
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TaggedVal {
    stamp: RoundStamp,
    payload: Val,
}

impl TaggedVal {
    fn new(stamp: RoundStamp, payload: Val) -> Self {
        Self { stamp, payload }
    }
}

fn expect_tagged_val(val: Val) -> TaggedVal {
    match val.as_any().downcast::<TaggedVal>() {
        Ok(v) => *v,
        Err(_) => {
            panic!(
                "wrong message return type; expecting TaggedVal but got {}",
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
