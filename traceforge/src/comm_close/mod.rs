use std::any::TypeId;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use crate::channel::{self_loc_comm, thread_loc_comm};
use crate::coverage::ExecutionId;
use crate::msg::{Message, Val};
use crate::predicate::PredicateType;
use crate::runtime::execution::ExecutionState;
use crate::thread::ThreadId;
use std::any::type_name;
use std::iter;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchKind {
    Eq,
    Gt,
    Gte,
    Any,
}

impl From<&str> for MatchKind {
    fn from(value: &str) -> Self {
        match value {
            "=" => MatchKind::Eq,
            ">" => MatchKind::Gt,
            ">=" => MatchKind::Gte,
            "*" => MatchKind::Any,
            _ => panic!(
                "invalid match comparator `{}`; expected one of \"=\", \">\", \">=\", \"*\"",
                value
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DimensionKind {
    U32,
    Enum { type_name: &'static str, size: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DimensionSpec {
    name: &'static str,
    kind: DimensionKind,
    low: u32,
    default_match: MatchKind,
}

impl DimensionSpec {
    pub fn u32(name: &'static str, default_match: MatchKind) -> Self {
        Self {
            name,
            kind: DimensionKind::U32,
            low: 0,
            default_match,
        }
    }

    pub fn enumeration<E: DimensionEnum>(name: &'static str, default_match: MatchKind) -> Self {
        Self {
            name,
            kind: DimensionKind::Enum {
                type_name: E::NAME,
                size: E::SIZE,
            },
            low: E::LOW.to_u32(),
            default_match,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn kind(&self) -> &DimensionKind {
        &self.kind
    }

    pub fn low(&self) -> u32 {
        self.low
    }

    pub fn default_match(&self) -> MatchKind {
        self.default_match
    }
}

pub trait DimensionEnum: Copy + Eq + 'static {
    const NAME: &'static str;
    const LOW: Self;
    const SIZE: u32;

    fn to_u32(self) -> u32;
    fn try_from_u32(v: u32) -> Option<Self>;

    fn next(self) -> Option<Self> {
        let next = self.to_u32().checked_add(1)?;
        Self::try_from_u32(next)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scheme {
    dimensions: Vec<DimensionSpec>,
}

impl Scheme {
    pub fn from_dimensions(dimensions: Vec<DimensionSpec>) -> Self {
        Self { dimensions }
    }

    pub fn dimensions(&self) -> &[DimensionSpec] {
        &self.dimensions
    }

    fn low_components(&self) -> Vec<u32> {
        self.dimensions.iter().map(DimensionSpec::low).collect()
    }
}

pub trait RoundDescriptor: Sized + 'static {
    fn scheme() -> Scheme;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dimension<S> {
    index: usize,
    _marker: PhantomData<S>,
}

impl<S> Dimension<S> {
    #[doc(hidden)]
    pub const fn new(index: usize) -> Self {
        Self {
            index,
            _marker: PhantomData,
        }
    }

    pub(crate) const fn index(self) -> usize {
        self.index
    }
}

thread_local! {
    static ROUND_STATE: RefCell<SharedRoundState> = RefCell::new(SharedRoundState::default());
}

#[derive(Debug, Default)]
struct SharedRoundState {
    execution_id: Option<ExecutionId>,
    must_id: Option<usize>,
    cursors: HashMap<(ThreadId, TypeId), Vec<u32>>,
}

fn with_shared_cursor<S, F, R>(scheme: &Scheme, f: F) -> R
where
    S: RoundDescriptor,
    F: FnOnce(&mut Vec<u32>) -> R,
{
    let (tid, eid, must_id) = ExecutionState::with(|state| {
        let must_id = Rc::as_ptr(&state.must) as usize;
        let must = state.must.borrow();
        let tid = must.to_thread_id(state.current().id());
        let eid = must.telemetry.coverage.current_eid();
        (tid, eid, must_id)
    });

    ROUND_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.must_id != Some(must_id) || state.execution_id != Some(eid) {
            state.must_id = Some(must_id);
            state.execution_id = Some(eid);
            state.cursors.clear();
        }

        let key = (tid, TypeId::of::<S>());
        let cursor = state
            .cursors
            .entry(key)
            .or_insert_with(|| scheme.low_components());
        f(cursor)
    })
}

fn with_existing_cursor<S, F, R>(f: F) -> R
where
    S: 'static,
    F: FnOnce(&Vec<u32>) -> R,
{
    let (tid, eid, must_id) = ExecutionState::with(|state| {
        let must_id = Rc::as_ptr(&state.must) as usize;
        let must = state.must.borrow();
        let tid = must.to_thread_id(state.current().id());
        let eid = must.telemetry.coverage.current_eid();
        (tid, eid, must_id)
    });

    ROUND_STATE.with(|state| {
        let state = state.borrow();
        assert!(
            state.must_id == Some(must_id) && state.execution_id == Some(eid),
            "round cursor state is not initialized for this execution"
        );
        let key = (tid, TypeId::of::<S>());
        let cursor = state
            .cursors
            .get(&key)
            .unwrap_or_else(|| panic!("round cursor is not initialized for this thread/type"));
        f(cursor)
    })
}

#[derive(Clone, Debug)]
pub struct Round<S> {
    components: Vec<u32>,
    _marker: PhantomData<S>,
}

impl<S> Round<S> {
    pub fn component(&self, dimension: Dimension<S>) -> u32 {
        let index = dimension.index();
        self.components[index]
    }

    pub fn components(&self) -> Vec<u32> {
        self.components.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoundStamp<S> {
    components: Vec<u32>,
    _marker: PhantomData<S>,
}

impl<S> RoundStamp<S> {
    pub fn components(&self) -> &[u32] {
        &self.components
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoundMsg<T, S> {
    payload: T,
    stamp: RoundStamp<S>,
}

impl<T, S: 'static> RoundMsg<T, S> {
    pub fn payload(&self) -> &T {
        assert_not_past::<S>(&self.stamp.components);
        &self.payload
    }

    pub fn stamp(&self) -> &RoundStamp<S> {
        &self.stamp
    }
}

#[derive(Clone, Debug)]
pub struct RoundFilter<S> {
    base: Vec<u32>,
    overrides: Vec<Option<MatchKind>>,
    _marker: PhantomData<S>,
}

impl<S> RoundFilter<S> {
    pub fn with_match(mut self, dimension: Dimension<S>, kind: MatchKind) -> Self {
        let index = dimension.index();
        self.overrides[index] = Some(kind);
        self
    }
}

#[derive(Clone, Debug)]
pub struct Rounds<S> {
    scheme: Scheme,
    use_tags: bool,
    _marker: PhantomData<S>,
}

impl<S: RoundDescriptor> Rounds<S> {
    pub fn new() -> Self {
        let scheme = S::scheme();
        with_shared_cursor::<S, _, _>(&scheme, |_| ());
        Self {
            scheme,
            use_tags: true,
            _marker: PhantomData,
        }
    }

    pub fn new_wo_tags(stash: bool) -> Self {
        assert!(
            !stash,
            "new_wo_tags(stash=true) is not supported; no-tag mode currently drops non-matching messages"
        );
        let scheme = S::scheme();
        with_shared_cursor::<S, _, _>(&scheme, |_| ());
        Self {
            scheme,
            use_tags: false,
            _marker: PhantomData,
        }
    }

    pub fn scheme(&self) -> &Scheme {
        &self.scheme
    }

    pub fn current(&self) -> Round<S> {
        with_shared_cursor::<S, _, _>(&self.scheme, |current| Round {
            components: current.clone(),
            _marker: PhantomData,
        })
    }

    pub fn filter(&self) -> RoundFilter<S> {
        let base = with_shared_cursor::<S, _, _>(&self.scheme, |current| current.clone());
        let overrides = vec![None; self.scheme.dimensions.len()];
        RoundFilter {
            base,
            overrides,
            _marker: PhantomData,
        }
    }

    pub fn advance(&mut self, dimension: Dimension<S>) -> Round<S> {
        let index = dimension.index();
        let level_count = self.scheme.dimensions.len();
        if index >= level_count {
            panic!("dimension index {} is out of range {}", index, level_count);
        }

        with_shared_cursor::<S, _, _>(&self.scheme, |current| {
            let spec = &self.scheme.dimensions[index];
            let next = match spec.kind() {
                DimensionKind::U32 => current[index].checked_add(1).expect("dimension overflow"),
                DimensionKind::Enum { size, .. } => {
                    let value = current[index];
                    let next = value.checked_add(1).expect("dimension overflow");
                    if next >= *size {
                        panic!(
                            "dimension {} has no next enum value after {}",
                            spec.name(),
                            value
                        );
                    }
                    next
                }
            };
            current[index] = next;
            for trailing in index + 1..level_count {
                current[trailing] = self.scheme.dimensions[trailing].low();
            }
            Round {
                components: current.clone(),
                _marker: PhantomData,
            }
        })
    }

    pub fn advance_to(&mut self, dimension: Dimension<S>, target: u32) {
        let index = dimension.index();
        let level_count = self.scheme.dimensions.len();
        if index >= level_count {
            panic!("dimension index {} is out of range {}", index, level_count);
        }

        with_shared_cursor::<S, _, _>(&self.scheme, |current| {
            let spec = &self.scheme.dimensions[index];
            if let DimensionKind::Enum { type_name, size } = spec.kind() {
                assert!(
                    target < *size,
                    "invalid value {} for enum dimension {} ({})",
                    target,
                    spec.name(),
                    type_name
                );
            }

            let value = current[index];
            assert!(
                target >= value,
                "dimension {} cannot decrease from {} to {}",
                spec.name(),
                value,
                target
            );

            if target == value {
                return;
            }

            current[index] = target;
            for trailing in index + 1..level_count {
                current[trailing] = self.scheme.dimensions[trailing].low();
            }
        });
    }

    pub fn jump(&mut self, stamp: &RoundStamp<S>) -> Round<S> {
        let level_count = self.scheme.dimensions.len();
        ensure_len("round stamp", level_count, stamp.components.len());
        with_shared_cursor::<S, _, _>(&self.scheme, |current| {
            let mut advanced = false;
            for (cur, incoming) in current.iter().zip(stamp.components.iter()) {
                if incoming > cur {
                    advanced = true;
                    break;
                } else if incoming < cur {
                    panic!(
                        "round stamp {:?} would decrement current round {:?}",
                        stamp.components, current
                    );
                }
            }

            if advanced {
                *current = stamp.components.clone();
            }

            Round {
                components: current.clone(),
                _marker: PhantomData,
            }
        })
    }

    pub fn jump_if_future(&mut self, stamp: &RoundStamp<S>) -> Round<S> {
        let level_count = self.scheme.dimensions.len();
        ensure_len("round stamp", level_count, stamp.components.len());
        with_shared_cursor::<S, _, _>(&self.scheme, |current| {
            let mut advanced = false;
            for (cur, incoming) in current.iter().zip(stamp.components.iter()) {
                if incoming > cur {
                    advanced = true;
                    break;
                } else if incoming < cur {
                    return Round {
                        components: current.clone(),
                        _marker: PhantomData,
                    };
                }
            }

            if advanced {
                *current = stamp.components.clone();
            }

            Round {
                components: current.clone(),
                _marker: PhantomData,
            }
        })
    }

    pub fn send<T: Message + 'static>(&self, tid: ThreadId, msg: T) {
        let components = with_shared_cursor::<S, _, _>(&self.scheme, |current| current.clone());
        let tagged = TaggedVal::new(components.clone(), Val::new(msg));
        let (loc, comm) = thread_loc_comm(tid);
        let tag = if self.use_tags { Some(components) } else { None };
        crate::send_msg_with_tag_vec(tagged, tag, &loc, comm, false);
    }

    pub fn recv<T: Message + 'static>(&self) -> Option<RoundMsg<T, S>> {
        let filter = self.filter();
        self.recv_with::<T>(&filter)
    }

    pub fn recv_with<T: Message + 'static>(&self, filter: &RoundFilter<S>) -> Option<RoundMsg<T, S>> {
        with_shared_cursor::<S, _, _>(&self.scheme, |current| {
            assert!(
                filter.base == *current,
                "filter is stale; build a fresh filter from rounds.filter() before recv_with"
            );
        });
        let (loc, comm) = self_loc_comm();
        if self.use_tags {
            let predicate = filter_tag_predicate(&self.scheme, &filter.base, &filter.overrides);
            let tagged: Option<TaggedVal> =
                crate::recv_msg_with_tag(iter::once(&loc), comm, Some(predicate)).map(|x| x.0);
            tagged.map(|tagged| RoundMsg {
                payload: expect_payload::<T>(tagged.payload),
                stamp: RoundStamp {
                    components: tagged.components,
                    _marker: PhantomData,
                },
            })
        } else {
            loop {
                let tagged: Option<TaggedVal> =
                    crate::recv_msg_with_tag(iter::once(&loc), comm, None).map(|x| x.0);
                let tagged = match tagged {
                    Some(tagged) => tagged,
                    None => return None,
                };
                if !matches_scheme_with_filter(
                    &self.scheme,
                    &tagged.components,
                    &filter.base,
                    &filter.overrides,
                ) {
                    continue;
                }
                return Some(RoundMsg {
                    payload: expect_payload::<T>(tagged.payload),
                    stamp: RoundStamp {
                        components: tagged.components,
                        _marker: PhantomData,
                    },
                });
            }
        }
    }

    pub fn recv_block<T: Message + 'static>(&self) -> RoundMsg<T, S> {
        let filter = self.filter();
        self.recv_block_with::<T>(&filter)
    }

    pub fn recv_block_with<T: Message + 'static>(&self, filter: &RoundFilter<S>) -> RoundMsg<T, S> {
        with_shared_cursor::<S, _, _>(&self.scheme, |current| {
            assert!(
                filter.base == *current,
                "filter is stale; build a fresh filter from rounds.filter() before recv_block_with"
            );
        });
        let (loc, comm) = self_loc_comm();
        if self.use_tags {
            let predicate = filter_tag_predicate(&self.scheme, &filter.base, &filter.overrides);
            let tagged: TaggedVal =
                crate::recv_msg_block_with_tag(iter::once(&loc), comm, Some(predicate)).0;
            RoundMsg {
                payload: expect_payload::<T>(tagged.payload),
                stamp: RoundStamp {
                    components: tagged.components,
                    _marker: PhantomData,
                },
            }
        } else {
            loop {
                let tagged: TaggedVal = crate::recv_msg_block_with_tag(iter::once(&loc), comm, None).0;
                if !matches_scheme_with_filter(
                    &self.scheme,
                    &tagged.components,
                    &filter.base,
                    &filter.overrides,
                ) {
                    continue;
                }
                return RoundMsg {
                    payload: expect_payload::<T>(tagged.payload),
                    stamp: RoundStamp {
                        components: tagged.components,
                        _marker: PhantomData,
                    },
                };
            }
        }
    }

    pub fn inbox<T: Message + 'static>(&self) -> Vec<Option<RoundMsg<T, S>>> {
        self.inbox_with_bounds::<T>(0, None)
    }

    pub fn inbox_with_bounds<T: Message + 'static>(
        &self,
        min: usize,
        max: Option<usize>,
    ) -> Vec<Option<RoundMsg<T, S>>> {
        let filter = self.filter();
        self.inbox_with_bounds_with::<T>(&filter, min, max)
    }

    pub fn inbox_with<T: Message + 'static>(&self, filter: &RoundFilter<S>) -> Vec<Option<RoundMsg<T, S>>> {
        self.inbox_with_bounds_with::<T>(filter, 0, None)
    }

    pub fn inbox_with_bounds_with<T: Message + 'static>(
        &self,
        filter: &RoundFilter<S>,
        min: usize,
        max: Option<usize>,
    ) -> Vec<Option<RoundMsg<T, S>>> {
        if !self.use_tags {
            panic!(
                "inbox/inbox_with_bounds/inbox_with_bounds_with are not supported in Rounds::new_wo_tags mode"
            );
        }
        with_shared_cursor::<S, _, _>(&self.scheme, |current| {
            assert!(
                filter.base == *current,
                "filter is stale; build a fresh filter from rounds.filter() before inbox_with_bounds_with"
            );
        });
        let predicate = filter_tag_predicate(&self.scheme, &filter.base, &filter.overrides);
        crate::inbox_extended(Some(predicate), min, max)
            .into_iter()
            .map(|val| {
                val.map(|val| {
                    let tagged = expect_tagged_val(val);
                    RoundMsg {
                        payload: expect_payload::<T>(tagged.payload),
                        stamp: RoundStamp {
                            components: tagged.components,
                            _marker: PhantomData,
                        },
                    }
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TaggedVal {
    components: Vec<u32>,
    payload: Val,
}

impl TaggedVal {
    fn new(components: Vec<u32>, payload: Val) -> Self {
        Self {
            components,
            payload,
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

fn filter_tag_predicate(
    scheme: &Scheme,
    round: &[u32],
    overrides: &[Option<MatchKind>],
) -> PredicateType {
    let scheme = scheme.clone();
    let round = round.to_vec();
    let overrides = overrides.to_vec();
    PredicateType(Arc::new(move |_tid, tag| {
        let tag = match tag {
            Some(tag) => tag,
            None => return false,
        };
        matches_scheme_with_filter(&scheme, &tag, &round, &overrides)
    }))
}

fn assert_not_past<S: 'static>(stamp: &[u32]) {
    with_existing_cursor::<S, _, _>(|current| {
        ensure_len("message stamp", current.len(), stamp.len());
        assert!(
            matches_lexicographic(stamp, current),
            "message stamp {:?} is from the past relative to current round {:?}",
            stamp,
            current
        );
    });
}

fn matches_scheme_with_filter(
    scheme: &Scheme,
    tag: &[u32],
    round: &[u32],
    overrides: &[Option<MatchKind>],
) -> bool {
    ensure_len("tag", scheme.dimensions.len(), tag.len());
    ensure_len("round", scheme.dimensions.len(), round.len());
    ensure_len("filter overrides", scheme.dimensions.len(), overrides.len());

    if !matches_lexicographic(tag, round) {
        return false;
    }

    for (index, spec) in scheme.dimensions.iter().enumerate() {
        let cmp = overrides[index].unwrap_or(spec.default_match());
        let ok = match cmp {
            MatchKind::Eq => tag[index] == round[index],
            MatchKind::Gt => tag[index] > round[index],
            MatchKind::Gte => tag[index] >= round[index],
            MatchKind::Any => true,
        };
        if !ok {
            return false;
        }
    }
    true
}

fn matches_lexicographic(tag: &[u32], round: &[u32]) -> bool {
    ensure_len("tag", round.len(), tag.len());
    ensure_len("round", tag.len(), round.len());
    for (t, r) in tag.iter().zip(round.iter()) {
        if t > r {
            return true;
        }
        if t < r {
            return false;
        }
    }
    true
}

fn ensure_len(label: &str, expected: usize, actual: usize) {
    if expected != actual {
        panic!(
            "{} length {} does not match scheme dimensions {}",
            label, actual, expected
        );
    }
}
