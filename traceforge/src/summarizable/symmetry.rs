use crate::msg::Message;
use crate::thread::ThreadId;
use dyn_clone::DynClone;
use std::any::Any;
use std::collections::HashMap;

#[cfg(feature = "symbolic")]
use crate::symbolic::{SymExpr, SymVarId};

/// A bijection from the participants of the current call to those of a stored summary case.
#[derive(Clone, Debug)]
pub struct ParticipantBijection {
    actual_to_representative: HashMap<ThreadId, ThreadId>,
    representative_to_actual: HashMap<ThreadId, ThreadId>,
}

impl ParticipantBijection {
    pub(crate) fn from_pairs(pairs: impl IntoIterator<Item = (ThreadId, ThreadId)>) -> Self {
        let mut actual_to_representative = HashMap::new();
        let mut representative_to_actual = HashMap::new();
        for (actual, representative) in pairs {
            assert!(
                actual_to_representative
                    .insert(actual, representative)
                    .is_none(),
                "participant mapping contains one actual thread twice"
            );
            assert!(
                representative_to_actual
                    .insert(representative, actual)
                    .is_none(),
                "participant mapping is not injective"
            );
        }
        Self {
            actual_to_representative,
            representative_to_actual,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.actual_to_representative.len()
    }

    /// Compare IDs while treating mapped participant IDs as bound names.
    pub fn input_ids_match(&self, actual: ThreadId, representative: ThreadId) -> bool {
        match self.actual_to_representative.get(&actual) {
            Some(expected) => *expected == representative,
            None => {
                !self.representative_to_actual.contains_key(&representative)
                    && actual == representative
            }
        }
    }

    /// Rename a stored participant ID back to the corresponding current participant.
    pub fn instantiate_id(&self, representative: ThreadId) -> ThreadId {
        self.representative_to_actual
            .get(&representative)
            .copied()
            .unwrap_or(representative)
    }

    pub(crate) fn representative_for(&self, actual: ThreadId) -> Option<ThreadId> {
        self.actual_to_representative.get(&actual).copied()
    }
}

#[doc(hidden)]
#[derive(Default)]
pub struct InputAbstraction {
    #[cfg(feature = "symbolic")]
    next_symbolic_input: usize,
}

impl InputAbstraction {
    #[cfg(feature = "symbolic")]
    fn abstract_symbolic(&mut self, value: &SymExpr) -> SymExpr {
        let index = self.next_symbolic_input;
        self.next_symbolic_input += 1;
        SymExpr::summary_input(index, value.sort())
    }
}

#[doc(hidden)]
pub struct MatchContext {
    participants: ParticipantBijection,
    #[cfg(feature = "symbolic")]
    symbolic_inputs: HashMap<SymVarId, SymExpr>,
}

impl MatchContext {
    pub(crate) fn new(participants: ParticipantBijection) -> Self {
        Self {
            participants,
            #[cfg(feature = "symbolic")]
            symbolic_inputs: HashMap::new(),
        }
    }

    pub(crate) fn participants(&self) -> &ParticipantBijection {
        &self.participants
    }

    #[cfg(feature = "symbolic")]
    fn match_symbolic(&mut self, actual: &SymExpr, stored: &SymExpr) -> bool {
        let SymExpr::Var { id, sort } = stored else {
            return false;
        };
        if id.input_index().is_none() || actual.sort() != sort.clone() {
            return false;
        }
        match self.symbolic_inputs.get(id) {
            Some(previous) => previous == actual,
            None => {
                self.symbolic_inputs.insert(id.clone(), actual.clone());
                true
            }
        }
    }

    pub(crate) fn into_instantiation(self) -> InstantiationContext {
        InstantiationContext {
            participants: self.participants,
            #[cfg(feature = "symbolic")]
            symbolic_values: self.symbolic_inputs,
            #[cfg(feature = "symbolic")]
            allow_unbound_locals: true,
            #[cfg(feature = "symbolic")]
            allow_execution_values: false,
            #[cfg(feature = "symbolic")]
            allow_unbound_inputs: false,
        }
    }
}

#[doc(hidden)]
#[derive(Clone)]
pub struct InstantiationContext {
    participants: ParticipantBijection,
    #[cfg(feature = "symbolic")]
    symbolic_values: HashMap<SymVarId, SymExpr>,
    #[cfg(feature = "symbolic")]
    allow_unbound_locals: bool,
    #[cfg(feature = "symbolic")]
    allow_execution_values: bool,
    #[cfg(feature = "symbolic")]
    allow_unbound_inputs: bool,
}

impl InstantiationContext {
    pub(crate) fn participants(&self) -> &ParticipantBijection {
        &self.participants
    }

    #[cfg(feature = "symbolic")]
    pub(crate) fn for_materialization(
        participants: &[ThreadId],
        values: impl IntoIterator<Item = (SymVarId, SymExpr)>,
        allow_unbound_inputs: bool,
    ) -> Self {
        Self {
            participants: ParticipantBijection::from_pairs(
                participants.iter().copied().map(|tid| (tid, tid)),
            ),
            symbolic_values: values.into_iter().collect(),
            allow_unbound_locals: false,
            allow_execution_values: true,
            allow_unbound_inputs,
        }
    }

    #[cfg(feature = "symbolic")]
    pub(crate) fn instantiate_symbolic(&self, value: &SymExpr) -> SymExpr {
        value.rewrite_vars(&mut |id, sort| {
            if let Some(replacement) = self.symbolic_values.get(id) {
                return replacement.clone();
            }
            if id.input_index().is_some() {
                if self.allow_unbound_inputs {
                    return SymExpr::Var {
                        id: id.clone(),
                        sort: sort.clone(),
                    };
                }
                panic!("summary input has no application binding");
            }
            if id.local_index().is_some() && self.allow_unbound_locals {
                return SymExpr::Var {
                    id: id.clone(),
                    sort: sort.clone(),
                };
            }
            if id.local_index().is_some() {
                panic!("summary local has no materialization binding");
            }
            if self.allow_execution_values {
                return SymExpr::Var {
                    id: id.clone(),
                    sort: sort.clone(),
                };
            }
            panic!("stored summary contains an execution-scoped symbolic variable");
        })
    }
}

#[doc(hidden)]
pub struct OutputNormalization {
    #[cfg(feature = "symbolic")]
    symbolic_locals: HashMap<SymVarId, SymExpr>,
}

impl OutputNormalization {
    #[cfg(feature = "symbolic")]
    pub(crate) fn new(symbolic_locals: HashMap<SymVarId, SymExpr>) -> Self {
        Self { symbolic_locals }
    }

    #[cfg(feature = "symbolic")]
    pub(crate) fn normalize_symbolic(&self, value: &SymExpr) -> SymExpr {
        value.rewrite_vars(&mut |id, sort| {
            if id.input_index().is_some() {
                return SymExpr::Var {
                    id: id.clone(),
                    sort: sort.clone(),
                };
            }
            if let Some(local) = self.symbolic_locals.get(id) {
                return local.clone();
            }
            if id.local_index().is_some() {
                panic!("summary output contains a pre-normalized local");
            }
            panic!(
                "symbolic summary depends on execution variable {:?} outside its arguments and locals",
                id
            );
        })
    }
}

/// A value that can cross a summarizable-function boundary.
///
/// Values compare exactly when symmetry is disabled. When it is enabled, these
/// operations compare and instantiate values modulo participant renaming.
pub trait SummarizableVal: Clone + PartialEq + 'static {
    fn abstract_input(&self, context: &mut InputAbstraction) -> Self;

    fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool;

    fn instantiate(&self, context: &InstantiationContext) -> Self;

    fn normalize_summary_output(&self, context: &OutputNormalization) -> Self;
}

impl SummarizableVal for ThreadId {
    fn abstract_input(&self, _: &mut InputAbstraction) -> Self {
        *self
    }

    fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        context.participants().input_ids_match(*self, *stored)
    }

    fn instantiate(&self, context: &InstantiationContext) -> Self {
        context.participants().instantiate_id(*self)
    }

    fn normalize_summary_output(&self, _: &OutputNormalization) -> Self {
        *self
    }
}

macro_rules! unchanged {
    ($($ty:ty),* $(,)?) => {$(
        impl SummarizableVal for $ty {
            fn abstract_input(&self, _: &mut InputAbstraction) -> Self {
                self.clone()
            }

            fn matches_stored_input(
                &self,
                stored: &Self,
                _: &mut MatchContext,
            ) -> bool {
                self == stored
            }

            fn instantiate(&self, _: &InstantiationContext) -> Self {
                self.clone()
            }

            fn normalize_summary_output(&self, _: &OutputNormalization) -> Self {
                self.clone()
            }
        }
    )*};
}

unchanged!(
    (),
    bool,
    char,
    String,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    f32,
    f64,
);

#[cfg(feature = "symbolic")]
impl SummarizableVal for SymExpr {
    fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
        context.abstract_symbolic(self)
    }

    fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        context.match_symbolic(self, stored)
    }

    fn instantiate(&self, context: &InstantiationContext) -> Self {
        context.instantiate_symbolic(self)
    }

    fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
        context.normalize_symbolic(self)
    }
}

impl<T: SummarizableVal> SummarizableVal for Option<T> {
    fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
        self.as_ref().map(|value| value.abstract_input(context))
    }

    fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        match (self, stored) {
            (Some(actual), Some(stored)) => actual.matches_stored_input(stored, context),
            (None, None) => true,
            _ => false,
        }
    }

    fn instantiate(&self, context: &InstantiationContext) -> Self {
        self.as_ref().map(|value| value.instantiate(context))
    }

    fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
        self.as_ref()
            .map(|value| value.normalize_summary_output(context))
    }
}

impl<T: SummarizableVal, E: SummarizableVal> SummarizableVal for Result<T, E> {
    fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
        match self {
            Ok(value) => Ok(value.abstract_input(context)),
            Err(value) => Err(value.abstract_input(context)),
        }
    }

    fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        match (self, stored) {
            (Ok(actual), Ok(stored)) => actual.matches_stored_input(stored, context),
            (Err(actual), Err(stored)) => actual.matches_stored_input(stored, context),
            _ => false,
        }
    }

    fn instantiate(&self, context: &InstantiationContext) -> Self {
        match self {
            Ok(value) => Ok(value.instantiate(context)),
            Err(value) => Err(value.instantiate(context)),
        }
    }

    fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
        match self {
            Ok(value) => Ok(value.normalize_summary_output(context)),
            Err(value) => Err(value.normalize_summary_output(context)),
        }
    }
}

impl<T: SummarizableVal> SummarizableVal for Vec<T> {
    fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
        self.iter()
            .map(|value| value.abstract_input(context))
            .collect()
    }

    fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        self.len() == stored.len()
            && self
                .iter()
                .zip(stored)
                .all(|(actual, stored)| actual.matches_stored_input(stored, context))
    }

    fn instantiate(&self, context: &InstantiationContext) -> Self {
        self.iter()
            .map(|value| value.instantiate(context))
            .collect()
    }

    fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
        self.iter()
            .map(|value| value.normalize_summary_output(context))
            .collect()
    }
}

impl<T: SummarizableVal, const N: usize> SummarizableVal for [T; N] {
    fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
        std::array::from_fn(|index| self[index].abstract_input(context))
    }

    fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        self.iter()
            .zip(stored)
            .all(|(actual, stored)| actual.matches_stored_input(stored, context))
    }

    fn instantiate(&self, context: &InstantiationContext) -> Self {
        std::array::from_fn(|index| self[index].instantiate(context))
    }

    fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
        std::array::from_fn(|index| self[index].normalize_summary_output(context))
    }
}

impl<T: SummarizableVal> SummarizableVal for Box<T> {
    fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
        Box::new(self.as_ref().abstract_input(context))
    }

    fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        self.as_ref().matches_stored_input(stored.as_ref(), context)
    }

    fn instantiate(&self, context: &InstantiationContext) -> Self {
        Box::new(self.as_ref().instantiate(context))
    }

    fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
        Box::new(self.as_ref().normalize_summary_output(context))
    }
}

macro_rules! tuple_summarizable_val {
    ($(($type:ident, $actual:ident, $stored:ident)),+ $(,)?) => {
        impl<$($type: SummarizableVal),+> SummarizableVal for ($($type,)+) {
            fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
                let ($($actual,)+) = self;
                ($($actual.abstract_input(context),)+)
            }

            fn matches_stored_input(
                &self,
                stored: &Self,
                context: &mut MatchContext,
            ) -> bool {
                let ($($actual,)+) = self;
                let ($($stored,)+) = stored;
                true $(&& $actual.matches_stored_input($stored, context))+
            }

            fn instantiate(&self, context: &InstantiationContext) -> Self {
                let ($($actual,)+) = self;
                ($($actual.instantiate(context),)+)
            }

            fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
                let ($($actual,)+) = self;
                ($($actual.normalize_summary_output(context),)+)
            }
        }
    };
}

tuple_summarizable_val!((A, a, a_stored));
tuple_summarizable_val!((A, a, a_stored), (B, b, b_stored));
tuple_summarizable_val!((A, a, a_stored), (B, b, b_stored), (C, c, c_stored));
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored)
);
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored),
    (E, e, e_stored)
);
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored),
    (E, e, e_stored),
    (F, f, f_stored)
);
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored),
    (E, e, e_stored),
    (F, f, f_stored),
    (G, g, g_stored)
);
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored),
    (E, e, e_stored),
    (F, f, f_stored),
    (G, g, g_stored),
    (H, h, h_stored)
);
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored),
    (E, e, e_stored),
    (F, f, f_stored),
    (G, g, g_stored),
    (H, h, h_stored),
    (I, i, i_stored)
);
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored),
    (E, e, e_stored),
    (F, f, f_stored),
    (G, g, g_stored),
    (H, h, h_stored),
    (I, i, i_stored),
    (J, j, j_stored)
);
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored),
    (E, e, e_stored),
    (F, f, f_stored),
    (G, g, g_stored),
    (H, h, h_stored),
    (I, i, i_stored),
    (J, j, j_stored),
    (K, k, k_stored)
);
tuple_summarizable_val!(
    (A, a, a_stored),
    (B, b, b_stored),
    (C, c, c_stored),
    (D, d, d_stored),
    (E, e, e_stored),
    (F, f, f_stored),
    (G, g, g_stored),
    (H, h, h_stored),
    (I, i, i_stored),
    (J, j, j_stored),
    (K, k, k_stored),
    (L, l, l_stored)
);

/// Object-safe operations used to store a dynamically typed summarizable value.
trait DynSummarizableVal: Send + DynClone {
    fn as_any_ref(&self) -> &dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
    fn exact_eq(&self, other: &dyn DynSummarizableVal) -> bool;
    fn abstract_input_box(&self, context: &mut InputAbstraction) -> Box<dyn DynSummarizableVal>;
    fn matches_stored_input(
        &self,
        stored: &dyn DynSummarizableVal,
        context: &mut MatchContext,
    ) -> bool;
    fn instantiate_box(&self, context: &InstantiationContext) -> Box<dyn DynSummarizableVal>;
    #[cfg(feature = "symbolic")]
    fn normalize_summary_output_box(
        &self,
        context: &OutputNormalization,
    ) -> Box<dyn DynSummarizableVal>;
}

impl<T> DynSummarizableVal for T
where
    T: Message + SummarizableVal + Send + 'static,
{
    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }

    fn exact_eq(&self, other: &dyn DynSummarizableVal) -> bool {
        other
            .as_any_ref()
            .downcast_ref::<T>()
            .is_some_and(|value| self == value)
    }

    fn abstract_input_box(&self, context: &mut InputAbstraction) -> Box<dyn DynSummarizableVal> {
        Box::new(SummarizableVal::abstract_input(self, context))
    }

    fn matches_stored_input(
        &self,
        stored: &dyn DynSummarizableVal,
        context: &mut MatchContext,
    ) -> bool {
        stored
            .as_any_ref()
            .downcast_ref::<T>()
            .is_some_and(|value| SummarizableVal::matches_stored_input(self, value, context))
    }

    fn instantiate_box(&self, context: &InstantiationContext) -> Box<dyn DynSummarizableVal> {
        Box::new(SummarizableVal::instantiate(self, context))
    }

    #[cfg(feature = "symbolic")]
    fn normalize_summary_output_box(
        &self,
        context: &OutputNormalization,
    ) -> Box<dyn DynSummarizableVal> {
        Box::new(SummarizableVal::normalize_summary_output(self, context))
    }
}

dyn_clone::clone_trait_object!(DynSummarizableVal);

/// The type-erased internal representation of a value crossing a
/// summarizable-function boundary.
#[doc(hidden)]
#[derive(Clone)]
pub struct ErasedSummarizableVal {
    value: Box<dyn DynSummarizableVal>,
    type_name: &'static str,
}

impl ErasedSummarizableVal {
    pub fn new<T>(value: T) -> Self
    where
        T: Message + SummarizableVal + Send + 'static,
    {
        Self {
            value: Box::new(value),
            type_name: std::any::type_name::<T>(),
        }
    }

    pub(crate) fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
        Self {
            value: self.value.abstract_input_box(context),
            type_name: self.type_name,
        }
    }

    pub(crate) fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        self.type_name == stored.type_name
            && self.value.matches_stored_input(&*stored.value, context)
    }

    pub(crate) fn instantiate(&self, context: &InstantiationContext) -> Self {
        Self {
            value: self.value.instantiate_box(context),
            type_name: self.type_name,
        }
    }

    #[cfg(feature = "symbolic")]
    pub(crate) fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
        Self {
            value: self.value.normalize_summary_output_box(context),
            type_name: self.type_name,
        }
    }

    pub(crate) fn into_typed<T>(self) -> T
    where
        T: Message + SummarizableVal + Send + 'static,
    {
        *self.value.into_any().downcast::<T>().unwrap_or_else(|_| {
            panic!(
                "summary return type mismatch: expected {}, got {}",
                std::any::type_name::<T>(),
                self.type_name
            )
        })
    }
}

impl PartialEq for ErasedSummarizableVal {
    fn eq(&self, other: &Self) -> bool {
        self.type_name == other.type_name && self.value.exact_eq(&*other.value)
    }
}

impl std::fmt::Debug for ErasedSummarizableVal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ErasedSummarizableVal")
            .field("type_name", &self.type_name)
            .finish_non_exhaustive()
    }
}

/// The ordered arguments supplied by one participant at a summarizable boundary.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub struct SummarizableArguments(Vec<ErasedSummarizableVal>);

#[doc(hidden)]
pub struct SummarizableArgumentCursor {
    values: std::vec::IntoIter<ErasedSummarizableVal>,
}

impl SummarizableArguments {
    pub fn new(values: Vec<ErasedSummarizableVal>) -> Self {
        Self(values)
    }

    pub(crate) fn abstract_input(&self, context: &mut InputAbstraction) -> Self {
        Self(
            self.0
                .iter()
                .map(|value| value.abstract_input(context))
                .collect(),
        )
    }

    pub(crate) fn matches_stored_input(&self, stored: &Self, context: &mut MatchContext) -> bool {
        self.0.len() == stored.0.len()
            && self
                .0
                .iter()
                .zip(&stored.0)
                .all(|(actual, stored)| actual.matches_stored_input(stored, context))
    }

    pub fn into_cursor(self) -> SummarizableArgumentCursor {
        SummarizableArgumentCursor {
            values: self.0.into_iter(),
        }
    }
}

impl SummarizableArgumentCursor {
    pub fn next<T>(&mut self) -> T
    where
        T: Message + SummarizableVal + Send + 'static,
    {
        self.values
            .next()
            .expect("summarizable body argument is missing")
            .into_typed::<T>()
    }

    pub fn finish(self) {
        assert_eq!(
            self.values.len(),
            0,
            "summarizable body received too many arguments"
        );
    }
}
