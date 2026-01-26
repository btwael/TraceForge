use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::coverage::ExecutionId;
use crate::runtime::execution::ExecutionState;
use crate::thread::ThreadId;

thread_local! {
    static ROUND_STATE: RefCell<RoundState> = RefCell::new(RoundState::default());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TagCmp {
    Eq,
    Gte,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct LevelSpec {
    pub name: Option<&'static str>,
    pub default_cmp: TagCmp,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct RoundScheme {
    levels: Vec<LevelSpec>,
}

impl RoundScheme {
    /// Build the legacy single-level scheme (round >=).
    pub fn legacy() -> Self {
        Self {
            levels: vec![LevelSpec {
                name: Some("round"),
                default_cmp: TagCmp::Gte,
            }],
        }
    }

    /// Start building a custom multi-level scheme.
    pub fn builder() -> RoundSchemeBuilder {
        RoundSchemeBuilder::new()
    }

    /// Return the configured level specifications.
    pub fn levels(&self) -> &[LevelSpec] {
        &self.levels
    }

    /// Return the number of tag levels in the scheme.
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }
}

pub struct RoundSchemeBuilder {
    levels: Vec<LevelSpec>,
}

impl RoundSchemeBuilder {
    /// Create an empty scheme builder.
    pub fn new() -> Self {
        Self { levels: Vec::new() }
    }

    /// Add a named level with its default comparison mode.
    pub fn level(mut self, name: &'static str, default_cmp: TagCmp) -> Self {
        self.levels.push(LevelSpec {
            name: Some(name),
            default_cmp,
        });
        self
    }

    /// Add an unnamed level with its default comparison mode.
    pub fn level_unnamed(mut self, default_cmp: TagCmp) -> Self {
        self.levels.push(LevelSpec {
            name: None,
            default_cmp,
        });
        self
    }

    /// Finalize the scheme.
    pub fn build(self) -> RoundScheme {
        RoundScheme { levels: self.levels }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct RoundId(Vec<u32>);

impl RoundId {
    /// Return the tag components for this round id.
    pub fn components(&self) -> &[u32] {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct RoundStamp(Vec<u32>);

impl RoundStamp {
    pub fn from_components(components: Vec<u32>) -> Self {
        Self(components)
    }

    /// Return the tag components for this stamp.
    pub fn components(&self) -> &[u32] {
        &self.0
    }

    /// Returns true if this stamp is strictly greater at `index` and
    /// all prior components are >= the corresponding ones in `other`.
    pub fn gt_at(&self, other: &RoundStamp, index: usize) -> bool {
        assert_eq!(
            self.0.len(),
            other.0.len(),
            "round stamp length mismatch"
        );
        assert!(index < self.0.len(), "round stamp index out of bounds");
        let prefix_ok = self.0[..index]
            .iter()
            .zip(&other.0[..index])
            .all(|(a, b)| a >= b);
        prefix_ok && self.0[index] > other.0[index]
    }
}

impl From<RoundId> for RoundStamp {
    fn from(id: RoundId) -> Self {
        RoundStamp(id.0)
    }
}

impl From<&RoundId> for RoundStamp {
    fn from(id: &RoundId) -> Self {
        RoundStamp(id.0.clone())
    }
}

impl From<RoundStamp> for RoundId {
    fn from(stamp: RoundStamp) -> Self {
        RoundId(stamp.0)
    }
}

impl From<&RoundStamp> for RoundId {
    fn from(stamp: &RoundStamp) -> Self {
        RoundId(stamp.0.clone())
    }
}

impl From<&Round> for RoundStamp {
    fn from(round: &Round) -> Self {
        RoundStamp(round.id.0.clone())
    }
}

#[derive(Clone, Debug)]
pub struct RoundFilter {
    scheme: Arc<RoundScheme>,
    thread: ThreadId,
    components: Vec<u32>,
    cmp_overrides: Vec<Option<TagCmp>>,
}

impl RoundFilter {
    pub fn level_cmp(mut self, index: usize, cmp: TagCmp) -> Self {
        let levels = self.scheme.level_count();
        if index >= levels {
            panic!(
                "level {} is out of range for {} round levels",
                index, levels
            );
        }
        let default_cmp = self.scheme.levels()[index].default_cmp;
        /*if default_cmp == TagCmp::Eq && cmp == TagCmp::Gte {
            panic!("cannot relax comparison at level {}", index);
        }*/
        self.cmp_overrides[index] = Some(cmp);
        self
    }

    pub fn eq_all(mut self) -> Self {
        let levels = self.scheme.level_count();
        for index in 0..levels {
            self = self.level_cmp(index, TagCmp::Eq);
        }
        self
    }

    pub fn components(&self) -> &[u32] {
        &self.components
    }

    pub(crate) fn cmp_overrides(&self) -> &[Option<TagCmp>] {
        &self.cmp_overrides
    }

    pub(crate) fn scheme(&self) -> &RoundScheme {
        &self.scheme
    }

    pub(crate) fn assert_current(&self) {
        with_round_state(&self.scheme, |state, tid| {
            let levels = self.scheme.level_count();
            let entry = entry_for_levels(state, tid, levels);
            assert!(
                tid == self.thread,
                "round filter belongs to thread {} but was used on thread {}",
                self.thread,
                tid
            );
            assert!(
                &self.components == entry,
                "round filter {:?} is not the current round {:?}",
                self.components,
                entry
            );
        });
    }
}

#[derive(Clone, Debug)]
pub struct Round {
    id: RoundId,
    thread: ThreadId,
    scheme: Arc<RoundScheme>,
}

impl Round {
    // Create a round token with its tag components and ownership metadata.
    fn new(id: Vec<u32>, thread: ThreadId, scheme: Arc<RoundScheme>) -> Self {
        Self {
            id: RoundId(id),
            thread,
            scheme,
        }
    }

    /// Return the round id.
    pub fn id(&self) -> &RoundId {
        &self.id
    }

    /// Return a stamp for this round that can be safely sent in messages.
    pub fn stamp(&self) -> RoundStamp {
        RoundStamp(self.id.0.clone())
    }

    /// Return a filter initialized with the scheme defaults for this round.
    pub fn filter(&self) -> RoundFilter {
        let levels = self.scheme.level_count();
        RoundFilter {
            scheme: Arc::clone(&self.scheme),
            thread: self.thread,
            components: self.id.0.clone(),
            cmp_overrides: vec![None; levels],
        }
    }

    /// Return a single component by index.
    pub fn level(&self, index: usize) -> u32 {
        self.id.0[index]
    }

    /// Return all tag components.
    pub fn components(&self) -> &[u32] {
        self.id.components()
    }

    /// Return the scheme associated with this round.
    pub fn scheme(&self) -> &RoundScheme {
        &self.scheme
    }

    /// Assert that this token matches the current thread and round vector.
    pub(crate) fn assert_current(&self) {
        with_round_state(&self.scheme, |state, tid| {
            let levels = self.scheme.level_count();
            let entry = entry_for_levels(state, tid, levels);
            assert!(
                tid == self.thread,
                "round token belongs to thread {} but was used on thread {}",
                self.thread,
                tid
            );
            assert!(
                &self.id.0 == entry,
                "round token {:?} is not the current round {:?}",
                self.id.0,
                entry
            );
        });
    }
}

#[derive(Clone)]
pub struct Rounds {
    scheme: Arc<RoundScheme>,
}

impl Rounds {
    /// Create a rounds tracker using the legacy single-level scheme.
    pub fn new() -> Self {
        Self::with_scheme(RoundScheme::legacy())
    }

    /// Create a rounds tracker using a custom scheme.
    pub fn with_scheme(scheme: RoundScheme) -> Self {
        Self {
            scheme: Arc::new(scheme),
        }
    }

    /// Return the scheme used by this tracker.
    pub fn scheme(&self) -> &RoundScheme {
        &self.scheme
    }

    /// Return the current round token for this thread.
    pub fn current(&self) -> Round {
        current_round(&self.scheme)
    }

    /// Advance the top-level round and reset lower levels.
    pub fn advance_round(&mut self) -> Round {
        self.advance_level(0)
    }

    /// Advance a specific level and reset all lower levels.
    pub fn advance_level(&mut self, level: usize) -> Round {
        advance_level(&self.scheme, level)
    }

    /// Jump forward to match a received round stamp.
    pub fn jump(&mut self, stamp: &RoundStamp) -> Round {
        jump_round(&self.scheme, stamp.components())
    }
}

#[derive(Debug, Default)]
struct RoundState {
    execution_id: Option<ExecutionId>,
    must_id: Option<usize>,
    rounds: HashMap<ThreadId, Vec<u32>>,
}

// Access per-thread round state, clearing it when the execution or Must changes.
fn with_round_state<F, R>(_scheme: &Arc<RoundScheme>, f: F) -> R
where
    F: FnOnce(&mut RoundState, ThreadId) -> R,
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
            state.rounds.clear();
        }
        f(&mut state, tid)
    })
}

// Get or initialize the per-thread round vector for a given level count.
fn entry_for_levels<'a>(
    state: &'a mut RoundState,
    tid: ThreadId,
    levels: usize,
) -> &'a mut Vec<u32> {
    let entry = state.rounds.entry(tid).or_insert_with(|| vec![0; levels]);
    if entry.len() != levels {
        *entry = vec![0; levels];
    }
    entry
}

// Fetch the current round token for the calling thread.
fn current_round(scheme: &Arc<RoundScheme>) -> Round {
    with_round_state(scheme, |state, tid| {
        let levels = scheme.level_count();
        let entry = entry_for_levels(state, tid, levels);
        Round::new(entry.clone(), tid, Arc::clone(scheme))
    })
}

// Increment one level and reset lower levels, returning the new round token.
fn advance_level(scheme: &Arc<RoundScheme>, level: usize) -> Round {
    with_round_state(scheme, |state, tid| {
        let levels = scheme.level_count();
        if level >= levels {
            panic!(
                "level {} is out of range for {} round levels",
                level, levels
            );
        }
        let entry = entry_for_levels(state, tid, levels);
        entry[level] = entry[level].checked_add(1).expect("round counter overflow");
        for v in entry.iter_mut().skip(level + 1) {
            *v = 0;
        }
        Round::new(entry.clone(), tid, Arc::clone(scheme))
    })
}

// Jump to a stamp, ensuring we never decrease the round vector.
fn jump_round(scheme: &Arc<RoundScheme>, stamp: &[u32]) -> Round {
    with_round_state(scheme, |state, tid| {
        let levels = scheme.level_count();
        if stamp.len() != levels {
            panic!(
                "round stamp length {} does not match scheme levels {}",
                stamp.len(),
                levels
            );
        }
        let entry = entry_for_levels(state, tid, levels);
        let mut advanced = false;
        for (index, &value) in stamp.iter().enumerate() {
            let current = entry[index];
            if value > current {
                advanced = true;
                break;
            } else if value < current {
                panic!(
                    "round stamp {:?} would decrement current round {:?}",
                    stamp, entry
                );
            }
        }
        if advanced {
            *entry = stamp.to_vec();
        }
        Round::new(entry.clone(), tid, Arc::clone(scheme))
    })
}
