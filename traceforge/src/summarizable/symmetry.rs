use crate::msg::Message;
use crate::thread::ThreadId;
use dyn_clone::DynClone;
use std::any::Any;
use std::collections::HashMap;

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

/// A value that can cross a summarizable-function boundary.
///
/// Values compare exactly when symmetry is disabled. When it is enabled, these
/// operations compare and instantiate values modulo participant renaming.
pub trait SummarizableVal: Clone + PartialEq + 'static {
    fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool;

    fn instantiate(&self, bijection: &ParticipantBijection) -> Self;
}

impl SummarizableVal for ThreadId {
    fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool {
        bijection.input_ids_match(*self, *representative)
    }

    fn instantiate(&self, bijection: &ParticipantBijection) -> Self {
        bijection.instantiate_id(*self)
    }
}

macro_rules! unchanged {
    ($($ty:ty),* $(,)?) => {$(
        impl SummarizableVal for $ty {
            fn equivalent_to_representative(
                &self,
                representative: &Self,
                _: &ParticipantBijection,
            ) -> bool {
                self == representative
            }

            fn instantiate(&self, _: &ParticipantBijection) -> Self {
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

impl<T: SummarizableVal> SummarizableVal for Option<T> {
    fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool {
        match (self, representative) {
            (Some(actual), Some(stored)) => actual.equivalent_to_representative(stored, bijection),
            (None, None) => true,
            _ => false,
        }
    }

    fn instantiate(&self, bijection: &ParticipantBijection) -> Self {
        self.as_ref().map(|value| value.instantiate(bijection))
    }
}

impl<T: SummarizableVal, E: SummarizableVal> SummarizableVal for Result<T, E> {
    fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool {
        match (self, representative) {
            (Ok(actual), Ok(stored)) => actual.equivalent_to_representative(stored, bijection),
            (Err(actual), Err(stored)) => actual.equivalent_to_representative(stored, bijection),
            _ => false,
        }
    }

    fn instantiate(&self, bijection: &ParticipantBijection) -> Self {
        match self {
            Ok(value) => Ok(value.instantiate(bijection)),
            Err(value) => Err(value.instantiate(bijection)),
        }
    }
}

impl<T: SummarizableVal> SummarizableVal for Vec<T> {
    fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool {
        self.len() == representative.len()
            && self
                .iter()
                .zip(representative)
                .all(|(actual, stored)| actual.equivalent_to_representative(stored, bijection))
    }

    fn instantiate(&self, bijection: &ParticipantBijection) -> Self {
        self.iter()
            .map(|value| value.instantiate(bijection))
            .collect()
    }
}

impl<T: SummarizableVal, const N: usize> SummarizableVal for [T; N] {
    fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool {
        self.iter()
            .zip(representative)
            .all(|(actual, stored)| actual.equivalent_to_representative(stored, bijection))
    }

    fn instantiate(&self, bijection: &ParticipantBijection) -> Self {
        std::array::from_fn(|index| self[index].instantiate(bijection))
    }
}

impl<T: SummarizableVal> SummarizableVal for Box<T> {
    fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool {
        self.as_ref()
            .equivalent_to_representative(representative.as_ref(), bijection)
    }

    fn instantiate(&self, bijection: &ParticipantBijection) -> Self {
        Box::new(self.as_ref().instantiate(bijection))
    }
}

macro_rules! tuple_summarizable_val {
    ($(($type:ident, $actual:ident, $stored:ident)),+ $(,)?) => {
        impl<$($type: SummarizableVal),+> SummarizableVal for ($($type,)+) {
            fn equivalent_to_representative(
                &self,
                representative: &Self,
                bijection: &ParticipantBijection,
            ) -> bool {
                let ($($actual,)+) = self;
                let ($($stored,)+) = representative;
                true $(&& $actual.equivalent_to_representative($stored, bijection))+
            }

            fn instantiate(&self, bijection: &ParticipantBijection) -> Self {
                let ($($actual,)+) = self;
                ($($actual.instantiate(bijection),)+)
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
    fn equivalent(
        &self,
        representative: &dyn DynSummarizableVal,
        bijection: &ParticipantBijection,
    ) -> bool;
    fn instantiate_box(&self, bijection: &ParticipantBijection) -> Box<dyn DynSummarizableVal>;
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

    fn equivalent(
        &self,
        representative: &dyn DynSummarizableVal,
        bijection: &ParticipantBijection,
    ) -> bool {
        representative
            .as_any_ref()
            .downcast_ref::<T>()
            .is_some_and(|value| self.equivalent_to_representative(value, bijection))
    }

    fn instantiate_box(&self, bijection: &ParticipantBijection) -> Box<dyn DynSummarizableVal> {
        Box::new(self.instantiate(bijection))
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

    pub(crate) fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool {
        self.type_name == representative.type_name
            && self.value.equivalent(&*representative.value, bijection)
    }

    pub(crate) fn instantiate(&self, bijection: &ParticipantBijection) -> Self {
        Self {
            value: self.value.instantiate_box(bijection),
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
        self.equivalent_to_representative(
            other,
            &ParticipantBijection::from_pairs(std::iter::empty()),
        )
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

impl SummarizableArguments {
    pub fn new(values: Vec<ErasedSummarizableVal>) -> Self {
        Self(values)
    }

    pub(crate) fn equivalent_to_representative(
        &self,
        representative: &Self,
        bijection: &ParticipantBijection,
    ) -> bool {
        self.0.len() == representative.0.len()
            && self
                .0
                .iter()
                .zip(&representative.0)
                .all(|(actual, stored)| actual.equivalent_to_representative(stored, bijection))
    }
}
