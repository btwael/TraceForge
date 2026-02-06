use std::any::TypeId;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::coverage::ExecutionId;
use crate::runtime::execution::ExecutionState;
use crate::thread::ThreadId;

/// To be derived by enums used to identify round/tag components.
pub trait RoundKey: Copy + Eq + 'static {
    const COUNT: usize;
    fn index(self) -> usize; // 0..COUNT-1, stable
    fn name(self) -> &'static str; // "Ballot", "Phase", ...
}

/// To be derived by enums used to represent bounded steps/phases.
pub trait RoundEnum: Copy + Eq + 'static {
    const NAME: &'static str;
    const LOW: Self; // reset value (⊥)
    const SIZE: u32; // number of variants
    fn to_u32(self) -> u32;
    fn try_from_u32(v: u32) -> Option<Self>;

    fn next(self) -> Option<Self> {
        let next = self.to_u32().checked_add(1)?;
        Self::try_from_u32(next)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DefaultMatch {
    Eq,
    Gte,
    Any,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RoundOrder {
    ComponentWise,
    Lexicographic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundStamp {
    comps: Vec<u32>,
}

impl RoundStamp {
    pub fn from_components(components: Vec<u32>) -> Self {
        Self { comps: components }
    }

    pub fn components(&self) -> &[u32] {
        &self.comps
    }
}

thread_local! {
    static ROUND_STATE: RefCell<RoundState> = RefCell::new(RoundState::default());
}

#[derive(Debug, Default)]
struct RoundState {
    execution_id: Option<ExecutionId>,
    must_id: Option<usize>,
    rounds: HashMap<ThreadId, Vec<u32>>,
}

fn with_round_state<F, R>(f: F) -> R
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

pub(crate) fn set_current_round_stamp(stamp: &RoundStamp) {
    with_round_state(|state, tid| {
        state.rounds.insert(tid, stamp.components().to_vec());
    })
}

pub(crate) fn current_round_stamp_for_send() -> RoundStamp {
    with_round_state(|state, tid| {
        let entry = state.rounds.get(&tid).unwrap_or_else(|| {
            panic!("current round is not initialized; call Rounds::current/advance before send")
        });
        RoundStamp::from_components(entry.clone())
    })
}

#[derive(Clone)]
pub struct RoundScheme {
    levels: Vec<LevelSpec>,
    key_pos: HashMap<(TypeId, u16), usize>,
    enum_codecs: HashMap<TypeId, EnumCodec>,
    default_round_order: RoundOrder,
    default_filter: Option<CompiledFilter>,
}

impl RoundScheme {
    pub fn builder() -> RoundSchemeBuilder {
        RoundSchemeBuilder::new()
    }

    pub fn lexicographic() -> RoundSchemeBuilder {
        RoundSchemeBuilder::new()
            .default_round_order(RoundOrder::Lexicographic)
            .default_filter(DefaultFilter::new().round_order(RoundOrder::Lexicographic))
    }

    pub fn component_wise() -> RoundSchemeBuilder {
        RoundSchemeBuilder::new()
            .default_round_order(RoundOrder::ComponentWise)
            .default_filter(DefaultFilter::new().round_order(RoundOrder::ComponentWise))
    }

    pub fn with_default_filter(filter: DefaultFilter) -> RoundSchemeBuilder {
        RoundSchemeBuilder::new().default_filter(filter)
    }

    pub fn levels(&self) -> &[LevelSpec] {
        &self.levels
    }

    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    pub fn level_for_key<K: RoundKey>(&self, key: K) -> &LevelSpec {
        let index = self.key_position(key);
        &self.levels[index]
    }

    pub fn enum_codec<E: RoundEnum>(&self) -> &EnumCodec {
        let key = TypeId::of::<E>();
        self.enum_codecs
            .get(&key)
            .unwrap_or_else(|| panic!("enum codec for {} is not registered", E::NAME))
    }

    pub fn default_round_order(&self) -> RoundOrder {
        self.default_round_order
    }

    pub(crate) fn default_filter(&self) -> Option<&CompiledFilter> {
        self.default_filter.as_ref()
    }

    pub(crate) fn key_position<K: RoundKey>(&self, key: K) -> usize {
        let key_index = round_key_index::<K>(key);
        let key_type = TypeId::of::<K>();
        self.key_pos
            .get(&(key_type, key_index))
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "round scheme missing level for key {} (index {})",
                    key.name(),
                    key.index()
                )
            })
    }

    fn low_components(&self) -> Vec<u32> {
        self.levels.iter().map(|level| level.low).collect()
    }
}

pub struct RoundSchemeBuilder {
    levels: Vec<LevelSpec>,
    key_pos: HashMap<(TypeId, u16), usize>,
    enum_codecs: HashMap<TypeId, EnumCodec>,
    default_round_order: RoundOrder,
    default_filter: Option<DefaultFilter>,
}

#[derive(Clone, Copy, Debug)]
struct KeySpec {
    key_type: TypeId,
    key_index: u16,
    key_name: &'static str,
}

#[derive(Clone, Debug)]
pub struct DefaultFilter {
    order: Option<RoundOrder>,
    matches: Vec<(KeySpec, DefaultMatch)>,
}

impl DefaultFilter {
    pub fn new() -> Self {
        Self {
            order: None,
            matches: Vec::new(),
        }
    }

    pub fn round_order(mut self, order: RoundOrder) -> Self {
        self.order = Some(order);
        self
    }

    pub fn level_match<K: RoundKey>(mut self, key: K, match_kind: DefaultMatch) -> Self {
        self.matches.push((key_spec(key), match_kind));
        self
    }

    fn compile(
        self,
        key_pos: &HashMap<(TypeId, u16), usize>,
        default_order: RoundOrder,
        level_count: usize,
    ) -> CompiledFilter {
        let mut matches = vec![DefaultMatch::Any; level_count];
        for (key, match_kind) in self.matches {
            let index = key_pos.get(&(key.key_type, key.key_index)).unwrap_or_else(|| {
                panic!(
                    "default filter references unknown key {} (index {})",
                    key.key_name, key.key_index
                )
            });
            matches[*index] = match_kind;
        }
        CompiledFilter {
            order: self.order.unwrap_or(default_order),
            matches,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledFilter {
    order: RoundOrder,
    matches: Vec<DefaultMatch>,
}

impl CompiledFilter {
    pub(crate) fn order(&self) -> RoundOrder {
        self.order
    }

    pub(crate) fn matches(&self) -> &[DefaultMatch] {
        &self.matches
    }
}

impl RoundSchemeBuilder {
    pub fn new() -> Self {
        Self {
            levels: Vec::new(),
            key_pos: HashMap::new(),
            enum_codecs: HashMap::new(),
            default_round_order: RoundOrder::Lexicographic,
            default_filter: None,
        }
    }

    pub fn default_round_order(mut self, order: RoundOrder) -> Self {
        self.default_round_order = order;
        self
    }

    pub fn default_filter(mut self, filter: DefaultFilter) -> Self {
        self.default_filter = Some(filter);
        self
    }

    pub fn u32_level(self, key: impl RoundKey) -> U32LevelBuilder {
        U32LevelBuilder {
            builder: self,
            key: key_spec(key),
            low: 0,
        }
    }

    pub fn from_u32(self, key: impl RoundKey) -> Self {
        let mut builder = self;
        builder.add_u32_level(key_spec(key), 0);
        builder
    }

    pub fn enum_level<E: RoundEnum>(self, key: impl RoundKey) -> EnumLevelBuilder<E> {
        EnumLevelBuilder {
            builder: self,
            key: key_spec(key),
            low: E::LOW,
        }
    }

    pub fn from_enum<E: RoundEnum>(self, key: impl RoundKey) -> Self {
        let mut builder = self;
        builder.add_enum_level::<E>(key_spec(key), E::LOW);
        builder
    }

    pub fn build(self) -> RoundScheme {
        let compiled_filter = self.default_filter.map(|filter| {
            filter.compile(&self.key_pos, self.default_round_order, self.levels.len())
        });
        RoundScheme {
            levels: self.levels,
            key_pos: self.key_pos,
            enum_codecs: self.enum_codecs,
            default_round_order: self.default_round_order,
            default_filter: compiled_filter,
        }
    }

    fn push_level(&mut self, level: LevelSpec) {
        let key = (level.key_type, level.key_index);
        if self.key_pos.contains_key(&key) {
            panic!(
                "round scheme already has a level for key {} (index {})",
                level.key_name, level.key_index
            );
        }
        let position = self.levels.len();
        self.key_pos.insert(key, position);
        self.levels.push(level);
    }

    fn add_u32_level(&mut self, key: KeySpec, low: u32) {
        let level = LevelSpec {
            key_type: key.key_type,
            key_index: key.key_index,
            key_name: key.key_name,
            kind: LevelKind::U32,
            low,
        };
        self.push_level(level);
    }

    fn add_enum_level<E: RoundEnum>(&mut self, key: KeySpec, low: E) {
        let enum_type = TypeId::of::<E>();
        let codec = self.enum_codecs.entry(enum_type).or_insert_with(enum_codec::<E>);
        let low_value = low.to_u32();
        if !(codec.validate)(low_value) {
            panic!(
                "enum level {} has invalid low value {} for enum {}",
                key.key_name,
                low_value,
                codec.name
            );
        }
        let level = LevelSpec {
            key_type: key.key_type,
            key_index: key.key_index,
            key_name: key.key_name,
            kind: LevelKind::Enum { enum_type },
            low: low_value,
        };
        self.push_level(level);
    }
}

pub struct U32LevelBuilder {
    builder: RoundSchemeBuilder,
    key: KeySpec,
    low: u32,
}

impl U32LevelBuilder {
    pub fn low(mut self, low: u32) -> Self {
        self.low = low;
        self
    }

    pub fn done(mut self) -> RoundSchemeBuilder {
        self.builder.add_u32_level(self.key, self.low);
        self.builder
    }
}

pub struct EnumLevelBuilder<E: RoundEnum> {
    builder: RoundSchemeBuilder,
    key: KeySpec,
    low: E,
}

impl<E: RoundEnum> EnumLevelBuilder<E> {
    pub fn low(mut self, low: E) -> Self {
        self.low = low;
        self
    }

    pub fn done(mut self) -> RoundSchemeBuilder {
        self.builder.add_enum_level::<E>(self.key, self.low);
        self.builder
    }
}

#[derive(Clone)]
pub struct Round {
    scheme: Arc<RoundScheme>,
    stamp: RoundStamp,
}

impl Round {
    pub fn stamp(&self) -> RoundStamp {
        self.stamp.clone()
    }

    pub fn components(&self) -> &[u32] {
        self.stamp.components()
    }

    pub fn scheme(&self) -> &RoundScheme {
        &self.scheme
    }

    pub fn get_u32<K: RoundKey>(&self, key: K) -> u32 {
        let index = self.scheme.key_position(key);
        self.stamp.comps[index]
    }

    pub fn get_enum<K: RoundKey, E: RoundEnum>(&self, key: K) -> E {
        let raw = self.get_u32(key);
        E::try_from_u32(raw).unwrap_or_else(|| {
            panic!(
                "round level {} has invalid enum value {} for enum {}",
                key.name(),
                raw,
                E::NAME
            )
        })
    }

    pub fn assert_current(&self) {
        let current = current_round_stamp_for_send();
        assert!(
            self.stamp.components() == current.components(),
            "round token {:?} is not the current round {:?}",
            self.stamp.components(),
            current.components()
        );
    }
}

#[derive(Clone)]
pub struct Rounds {
    scheme: Arc<RoundScheme>,
    now: RoundStamp,
}

impl Rounds {
    pub fn with_scheme(scheme: RoundScheme) -> Self {
        let scheme = Arc::new(scheme);
        let now = RoundStamp::from_components(scheme.low_components());
        set_current_round_stamp(&now);
        Self { scheme, now }
    }

    pub fn scheme(&self) -> &RoundScheme {
        &self.scheme
    }

    pub(crate) fn scheme_arc(&self) -> Arc<RoundScheme> {
        Arc::clone(&self.scheme)
    }

    pub fn current(&self) -> Round {
        set_current_round_stamp(&self.now);
        Round {
            scheme: Arc::clone(&self.scheme),
            stamp: self.now.clone(),
        }
    }

    pub fn view<'a>(&'a self, stamp: &'a RoundStamp) -> RoundStampView<'a> {
        RoundStampView {
            scheme: self.scheme.as_ref(),
            stamp,
        }
    }

    pub fn advance_round(&mut self) -> Round {
        self.advance_level_index(0)
    }

    pub fn advance<K: RoundKey>(&mut self, key: K) -> Round {
        let index = self.scheme.key_position(key);
        self.advance_level_index(index)
    }

    pub fn goto_u32<K: RoundKey>(&mut self, key: K, value: u32) -> Round {
        let index = self.scheme.key_position(key);
        let spec = &self.scheme.levels[index];
        if !matches!(spec.kind, LevelKind::U32) {
            panic!("round level {} is not a u32 level", key.name());
        }
        self.set_level_value(index, value)
    }

    pub fn goto_enum<K: RoundKey, E: RoundEnum>(&mut self, key: K, value: E) -> Round {
        let index = self.scheme.key_position(key);
        let spec = &self.scheme.levels[index];
        if !matches!(spec.kind, LevelKind::Enum { .. }) {
            panic!("round level {} is not an enum level", key.name());
        }
        self.set_level_value(index, value.to_u32())
    }

    pub fn jump(&mut self, stamp: &RoundStamp) -> Round {
        if stamp.components().len() != self.scheme.level_count() {
            panic!(
                "round stamp length {} does not match scheme levels {}",
                stamp.components().len(),
                self.scheme.level_count()
            );
        }
        let mut advanced = false;
        for (current, incoming) in self.now.comps.iter().zip(stamp.components()) {
            if incoming > current {
                advanced = true;
                break;
            } else if incoming < current {
                panic!(
                    "round stamp {:?} would decrement current round {:?}",
                    stamp.components(),
                    self.now.components()
                );
            }
        }
        if advanced {
            self.now = stamp.clone();
        }
        self.current()
    }

    pub fn jump_if_future(&mut self, stamp: &RoundStamp) -> Round {
        if stamp.components().len() != self.scheme.level_count() {
            panic!(
                "round stamp length {} does not match scheme levels {}",
                stamp.components().len(),
                self.scheme.level_count()
            );
        }
        let mut advanced = false;
        for (current, incoming) in self.now.comps.iter().zip(stamp.components()) {
            if incoming > current {
                advanced = true;
                break;
            } else if incoming < current {
                return self.current();
            }
        }
        if advanced {
            self.now = stamp.clone();
        }
        self.current()
    }

    pub(crate) fn advance_level_index(&mut self, level: usize) -> Round {
        let levels = self.scheme.level_count();
        if level >= levels {
            panic!("level {} is out of range for {} round levels", level, levels);
        }
        let spec = &self.scheme.levels[level];
        let next_value = match spec.kind {
            LevelKind::U32 => self.now.comps[level]
                .checked_add(1)
                .expect("round counter overflow"),
            LevelKind::Enum { enum_type } => {
                let codec = self
                    .scheme
                    .enum_codecs
                    .get(&enum_type)
                    .expect("enum codec missing for level");
                let current = self.now.comps[level];
                if !(codec.validate)(current) {
                    panic!(
                        "round level {} has invalid enum value {} for enum {}",
                        spec.key_name, current, codec.name
                    );
                }
                (codec.next)(current).unwrap_or_else(|| {
                    panic!(
                        "round level {} (enum {}) has no next value",
                        spec.key_name, codec.name
                    )
                })
            }
        };
        self.now.comps[level] = next_value;
        for (idx, level_spec) in self.scheme.levels.iter().enumerate().skip(level + 1) {
            self.now.comps[idx] = level_spec.low;
        }
        self.current()
    }

    fn set_level_value(&mut self, level: usize, value: u32) -> Round {
        let levels = self.scheme.level_count();
        if level >= levels {
            panic!("level {} is out of range for {} round levels", level, levels);
        }
        let current = self.now.comps[level];
        if value < current {
            panic!(
                "round level {} cannot decrease from {} to {}",
                level, current, value
            );
        }
        if value == current {
            for (idx, level_spec) in self.scheme.levels.iter().enumerate().skip(level + 1) {
                let low = level_spec.low;
                let current = self.now.comps[idx];
                if current != low {
                    panic!(
                        "round level {} cannot reset later level {} from {} to {} without advancing",
                        level, idx, current, low
                    );
                }
            }
        }
        self.now.comps[level] = value;
        for (idx, level_spec) in self.scheme.levels.iter().enumerate().skip(level + 1) {
            self.now.comps[idx] = level_spec.low;
        }
        self.current()
    }
}

pub struct RoundStampView<'a> {
    scheme: &'a RoundScheme,
    stamp: &'a RoundStamp,
}

impl<'a> RoundStampView<'a> {
    pub fn get_u32<K: RoundKey>(&self, key: K) -> u32 {
        let index = self.scheme.key_position(key);
        self.stamp
            .components()
            .get(index)
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "round stamp missing level {} (index {}), stamp length {}",
                    key.name(),
                    index,
                    self.stamp.components().len()
                )
            })
    }

    pub fn get_enum<K: RoundKey, E: RoundEnum>(&self, key: K) -> E {
        let raw = self.get_u32(key);
        E::try_from_u32(raw).unwrap_or_else(|| {
            panic!(
                "round stamp level {} has invalid enum value {} for enum {}",
                key.name(),
                raw,
                E::NAME
            )
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelSpec {
    pub key_type: TypeId,
    pub key_index: u16,
    pub key_name: &'static str,
    pub kind: LevelKind,
    pub low: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LevelKind {
    U32,
    Enum { enum_type: TypeId },
}

#[derive(Clone, Copy)]
pub struct EnumCodec {
    pub name: &'static str,
    pub low: u32,
    pub size: u32,
    pub next: fn(u32) -> Option<u32>,
    pub validate: fn(u32) -> bool,
}

fn round_key_index<K: RoundKey>(key: K) -> u16 {
    if key.index() >= K::COUNT {
        panic!(
            "round key {} index {} is out of bounds for {} keys",
            key.name(),
            key.index(),
            K::COUNT
        );
    }
    u16::try_from(key.index()).unwrap_or_else(|_| {
        panic!(
            "round key {} index {} does not fit in u16",
            key.name(),
            key.index()
        )
    })
}

fn key_spec<K: RoundKey>(key: K) -> KeySpec {
    KeySpec {
        key_type: TypeId::of::<K>(),
        key_index: round_key_index(key),
        key_name: key.name(),
    }
}

fn enum_codec<E: RoundEnum>() -> EnumCodec {
    EnumCodec {
        name: E::NAME,
        low: E::LOW.to_u32(),
        size: E::SIZE,
        next: enum_next::<E>,
        validate: enum_validate::<E>,
    }
}

fn enum_next<E: RoundEnum>(value: u32) -> Option<u32> {
    let current = E::try_from_u32(value)?;
    current.next().map(|next| next.to_u32())
}

fn enum_validate<E: RoundEnum>(value: u32) -> bool {
    E::try_from_u32(value).is_some()
}
