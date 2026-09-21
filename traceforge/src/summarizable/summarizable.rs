#[cfg(feature = "symbolic")]
use super::symmetry::OutputNormalization;
use super::symmetry::{
    ErasedSummarizableVal, InputAbstraction, InstantiationContext, MatchContext,
    ParticipantBijection, SummarizableArguments, SummarizableVal,
};
use crate::event::Event;
use crate::event_label::{Choice, MonitorSends, RecvMsg, SendMsg};
#[cfg(feature = "symbolic")]
use crate::event_label::{ConstraintEval, ConstraintKind};
use crate::loc::{CommunicationModel, Loc, RecvLoc, SendLoc};
use crate::msg::Message;
use crate::predicate::PredicateType;
use crate::runtime::execution::ExecutionState;
use crate::runtime::thread::switch;
use crate::thread::ThreadId;
use crate::Val;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

#[cfg(feature = "symbolic")]
use crate::symbolic::{self, SymExpr, SymSort, SymVarId};

/// Stable identity of a function annotated with `#[summarizable]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SummarizableFunctionId(String);

impl SummarizableFunctionId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn name(&self) -> &str {
        &self.0
    }
}

/// Identity of one resolved collective call to a summarizable function.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SummarizableCallId {
    pub function: SummarizableFunctionId,
    pub call_index: u64,
}

/// Options controlling function-summary storage and reuse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummarizationOptions {
    symmetry: bool,
}

impl SummarizationOptions {
    /// Enable summary reuse across participant groups related by thread renaming.
    pub fn with_symmetry(mut self, enabled: bool) -> Self {
        self.symmetry = enabled;
        self
    }

    pub(crate) fn symmetry(&self) -> bool {
        self.symmetry
    }
}

/// Identity of one thread's attempt to enter a summarizable function.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ParticipationOfferId {
    function: SummarizableFunctionId,
    participant: ThreadId,
    occurrence: u64,
}

/// Membership constraints supplied by one caller of a summarizable function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participants {
    mode: ParticipationMode,
    required: Vec<ThreadId>,
    exactly_one: Vec<Vec<ThreadId>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParticipationMode {
    Resolve,
    Join(ThreadId),
}

impl Participants {
    /// Make this invocation responsible for resolving and committing the call.
    pub fn resolve() -> Self {
        Self {
            mode: ParticipationMode::Resolve,
            required: Vec::new(),
            exactly_one: Vec::new(),
        }
    }

    /// Offer this invocation to the given resolver.
    pub fn join(resolver: ThreadId) -> Self {
        Self {
            mode: ParticipationMode::Join(resolver),
            required: Vec::new(),
            exactly_one: Vec::new(),
        }
    }

    /// Require every listed thread to join this call.
    pub fn require(mut self, participants: impl IntoIterator<Item = ThreadId>) -> Self {
        for participant in participants {
            if !self.required.contains(&participant) {
                self.required.push(participant);
            }
        }
        self
    }

    /// Require exactly one member of the supplied alternative set.
    pub fn exactly_one_of(mut self, participants: impl IntoIterator<Item = ThreadId>) -> Self {
        let mut alternatives = Vec::new();
        for participant in participants {
            if !alternatives.contains(&participant) {
                alternatives.push(participant);
            }
        }
        assert!(
            !alternatives.is_empty(),
            "exactly_one_of requires at least one participant"
        );
        self.exactly_one.push(alternatives);
        self
    }

    pub(crate) fn required(&self) -> &[ThreadId] {
        &self.required
    }

    pub(crate) fn exactly_one(&self) -> &[Vec<ThreadId>] {
        &self.exactly_one
    }

    fn resolver(&self, caller: ThreadId) -> ThreadId {
        match self.mode {
            ParticipationMode::Resolve => caller,
            ParticipationMode::Join(resolver) => resolver,
        }
    }

    fn is_resolver(&self) -> bool {
        matches!(self.mode, ParticipationMode::Resolve)
    }

    fn normalize_for(&mut self, caller: ThreadId) {
        if let ParticipationMode::Join(resolver) = self.mode {
            assert_ne!(
                resolver, caller,
                "a summarizable invocation cannot join itself"
            );
        }
        self.required.retain(|candidate| *candidate != caller);
        self.required.sort();
        self.required.dedup();
        for alternatives in &mut self.exactly_one {
            alternatives.sort();
            alternatives.dedup();
        }
        self.exactly_one.sort();
        self.exactly_one.dedup();
    }
}

/// Static function information emitted by the macro `#[summarizable]`.
#[derive(Clone, Copy, Debug)]
pub struct SummarizableFunctionDescriptor {
    name: &'static str,
}

impl SummarizableFunctionDescriptor {
    pub const fn new(name: &'static str) -> Self {
        Self { name }
    }

    pub fn id(self) -> SummarizableFunctionId {
        SummarizableFunctionId::new(self.name)
    }
}

/// Handle for one collective call to a summarizable function.
#[derive(Clone, Debug)]
pub struct SummarizableCallHandle {
    pub(crate) call: SummarizableCallId,
}

/// Tells the generated wrapper whether to execute the body or apply a summary.
pub enum SummaryDispatch {
    ExecuteBody {
        handle: SummarizableCallHandle,
        arguments: SummarizableArguments,
    },
    ApplySummary(SummarizableCallHandle),
}

/// A stable vector of values indexed by participant thread.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipantValues<T>(pub(crate) Vec<(ThreadId, T)>);

impl<T: Clone> ParticipantValues<T> {
    pub(crate) fn from_map(values: &BTreeMap<ThreadId, T>) -> Self {
        Self(
            values
                .iter()
                .map(|(&tid, value)| (tid, value.clone()))
                .collect(),
        )
    }

    pub(crate) fn value_for(&self, tid: ThreadId) -> Option<T> {
        self.0
            .iter()
            .find_map(|(candidate, value)| (*candidate == tid).then(|| value.clone()))
    }
}

pub(crate) type ParticipantArguments = ParticipantValues<SummarizableArguments>;
pub(crate) type ParticipantReturns = ParticipantValues<ErasedSummarizableVal>;

impl ParticipantValues<SummarizableArguments> {
    fn abstract_inputs(&self) -> Self {
        let mut context = InputAbstraction::default();
        Self(
            self.0
                .iter()
                .map(|(participant, arguments)| {
                    (*participant, arguments.abstract_input(&mut context))
                })
                .collect(),
        )
    }

    fn matches_stored_inputs(&self, stored: &Self, context: &mut MatchContext) -> bool {
        if self.0.len() != stored.0.len() {
            return false;
        }

        self.0.iter().all(|(actual_tid, actual_arguments)| {
            let Some(representative_tid) = context.participants().representative_for(*actual_tid)
            else {
                return false;
            };
            stored
                .value_for(representative_tid)
                .is_some_and(|stored_arguments| {
                    actual_arguments.matches_stored_input(&stored_arguments, context)
                })
        })
    }
}

impl ParticipantValues<ErasedSummarizableVal> {
    fn instantiate(&self, context: &InstantiationContext) -> Self {
        let mut actual = BTreeMap::new();
        for (representative_tid, value) in &self.0 {
            let actual_tid = context.participants().instantiate_id(*representative_tid);
            assert!(
                actual
                    .insert(actual_tid, value.instantiate(context))
                    .is_none(),
                "summary instantiation mapped two returns to one participant"
            );
        }
        Self::from_map(&actual)
    }

    #[cfg(feature = "symbolic")]
    pub(crate) fn normalize_summary_output(&self, context: &OutputNormalization) -> Self {
        Self(
            self.0
                .iter()
                .map(|(participant, value)| (*participant, value.normalize_summary_output(context)))
                .collect(),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SummaryCondition {
    Always,
    #[cfg(feature = "symbolic")]
    Symbolic {
        guard: SymExpr,
        local_sorts: Vec<SymSort>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SummaryResult {
    Returned(ParticipantReturns),
    Blocked,
    AssumptionFailed,
}

/// One globally observable, optionally guarded outcome of a summarizable call.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SummaryOutcome {
    pub(crate) condition: SummaryCondition,
    pub(crate) result: SummaryResult,
}

impl SummaryOutcome {
    pub(crate) fn ordinary(result: SummaryResult) -> Self {
        Self {
            condition: SummaryCondition::Always,
            result,
        }
    }

    fn instantiate(&self, context: &InstantiationContext) -> Self {
        let condition = match &self.condition {
            SummaryCondition::Always => SummaryCondition::Always,
            #[cfg(feature = "symbolic")]
            SummaryCondition::Symbolic { guard, local_sorts } => SummaryCondition::Symbolic {
                guard: context.instantiate_symbolic(guard),
                local_sorts: local_sorts.clone(),
            },
        };
        let result = match &self.result {
            SummaryResult::Returned(values) => SummaryResult::Returned(values.instantiate(context)),
            SummaryResult::Blocked => SummaryResult::Blocked,
            SummaryResult::AssumptionFailed => SummaryResult::AssumptionFailed,
        };
        Self { condition, result }
    }

    #[cfg(feature = "symbolic")]
    pub(crate) fn materialize(self, participants: &[ThreadId], allow_parent_inputs: bool) -> Self {
        let local_sorts = match &self.condition {
            SummaryCondition::Always => return self,
            SummaryCondition::Symbolic { local_sorts, .. } => local_sorts.clone(),
        };
        let values = local_sorts.iter().enumerate().map(|(index, sort)| {
            (
                SymVarId::summary_local(index),
                symbolic::fresh(sort.clone()),
            )
        });
        let context =
            InstantiationContext::for_materialization(participants, values, allow_parent_inputs);
        self.instantiate(&context)
    }

    #[cfg(not(feature = "symbolic"))]
    pub(crate) fn materialize(self, _: &[ThreadId], _: bool) -> Self {
        self
    }

    pub(crate) fn is_assumption_failed(&self) -> bool {
        matches!(self.result, SummaryResult::AssumptionFailed)
    }

    #[cfg(feature = "symbolic")]
    pub(crate) fn guard(&self) -> Option<&SymExpr> {
        match &self.condition {
            SummaryCondition::Always => None,
            SummaryCondition::Symbolic { guard, .. } => Some(guard),
        }
    }
}

pub(crate) fn insert_summary_outcome(outcomes: &mut Vec<SummaryOutcome>, incoming: SummaryOutcome) {
    for existing in outcomes.iter_mut() {
        if existing.result != incoming.result {
            continue;
        }
        #[cfg(not(feature = "symbolic"))]
        return;

        #[cfg(feature = "symbolic")]
        match (&mut existing.condition, &incoming.condition) {
            (SummaryCondition::Always, _) => return,
            (condition, SummaryCondition::Always) => {
                *condition = SummaryCondition::Always;
                return;
            }
            (
                SummaryCondition::Symbolic {
                    guard: existing_guard,
                    local_sorts: existing_locals,
                },
                SummaryCondition::Symbolic {
                    guard: incoming_guard,
                    local_sorts: incoming_locals,
                },
            ) if existing_locals == incoming_locals => {
                *existing_guard = existing_guard.clone().or(incoming_guard.clone());
                return;
            }
            _ => {}
        }
    }
    outcomes.push(incoming);
}

#[derive(Clone, Debug)]
struct SummaryCase {
    participants: Vec<ThreadId>,
    inputs: ParticipantArguments,
    outcomes: Vec<SummaryOutcome>,
}

impl SummaryCase {
    fn instantiate_for(
        &self,
        actual_participants: &[ThreadId],
        actual_inputs: &ParticipantArguments,
        symmetry: bool,
    ) -> Option<Vec<SummaryOutcome>> {
        let mut outcomes = Vec::new();
        let bijections = if symmetry {
            participant_bijections(actual_participants, &self.participants)
        } else if actual_participants == self.participants {
            vec![ParticipantBijection::from_pairs(
                actual_participants
                    .iter()
                    .copied()
                    .zip(self.participants.iter().copied()),
            )]
        } else {
            Vec::new()
        };
        for bijection in bijections {
            let mut context = MatchContext::new(bijection);
            if !actual_inputs.matches_stored_inputs(&self.inputs, &mut context) {
                continue;
            }
            let context = context.into_instantiation();
            for outcome in &self.outcomes {
                let instantiated = outcome.instantiate(&context);
                insert_summary_outcome(&mut outcomes, instantiated);
            }
        }
        (!outcomes.is_empty()).then_some(outcomes)
    }
}

#[derive(Clone, Debug)]
enum CallMode {
    ExploringBody,
    ApplyingSummary {
        outcomes: Vec<SummaryOutcome>,
        selection: Option<(SummaryOutcome, Event)>,
    },
}

#[derive(Clone, Debug)]
struct SummarizableCallState {
    participants: Vec<ThreadId>,
    arguments: ParticipantArguments,
    selector: ThreadId,
    arrived_participants: Vec<ThreadId>,
    waiting_participants: Vec<ThreadId>,
    returns_by_participant: BTreeMap<ThreadId, ErasedSummarizableVal>,
    body_arguments: Option<ParticipantArguments>,
    mode: Option<CallMode>,
}

impl SummarizableCallState {
    fn new(
        participants: Vec<ThreadId>,
        arguments: ParticipantArguments,
        selector: ThreadId,
    ) -> Self {
        Self {
            participants,
            arguments,
            selector,
            arrived_participants: Vec::new(),
            waiting_participants: Vec::new(),
            returns_by_participant: BTreeMap::new(),
            body_arguments: None,
            mode: None,
        }
    }

    fn participant_count(&self) -> usize {
        self.participants.len()
    }
}

#[derive(Debug)]
pub(crate) enum EntryAction {
    WaitForParticipants,
    ExecuteBody {
        handle: SummarizableCallHandle,
        arguments: SummarizableArguments,
    },
    SelectSummaryOutcome {
        handle: SummarizableCallHandle,
    },
    ApplySummary(SummarizableCallHandle),
}

#[derive(Clone, Debug)]
pub(crate) struct SummaryMiss {
    pub(crate) participants: Vec<ThreadId>,
    pub(crate) actual_inputs: ParticipantArguments,
    pub(crate) summary_inputs: ParticipantArguments,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipationOffer {
    id: ParticipationOfferId,
    resolver: ThreadId,
    function: SummarizableFunctionId,
    depth: usize,
    parent: Option<SummarizableCallId>,
    specification: Participants,
    arguments: SummarizableArguments,
}

#[derive(Clone, Debug, PartialEq)]
struct ParticipationGrant {
    offer: ParticipationOfferId,
    call: SummarizableCallId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ParticipationDomain {
    function: SummarizableFunctionId,
    depth: usize,
    parent: Option<SummarizableCallId>,
    resolver: ThreadId,
}

impl ParticipationOffer {
    fn domain(&self) -> ParticipationDomain {
        ParticipationDomain {
            function: self.function.clone(),
            depth: self.depth,
            parent: self.parent.clone(),
            resolver: self.resolver,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SummarizationRendezvousLoc {
    Offers(ParticipationDomain),
    Grant(ParticipationOfferId),
}

fn permutations<T: Copy>(values: &[T]) -> Vec<Vec<T>> {
    fn visit<T: Copy>(values: &mut Vec<T>, at: usize, output: &mut Vec<Vec<T>>) {
        if at == values.len() {
            output.push(values.clone());
            return;
        }
        for selected in at..values.len() {
            values.swap(at, selected);
            visit(values, at + 1, output);
            values.swap(at, selected);
        }
    }

    let mut values = values.to_vec();
    let mut output = Vec::new();
    visit(&mut values, 0, &mut output);
    output
}

fn participant_bijections(
    actual: &[ThreadId],
    representatives: &[ThreadId],
) -> Vec<ParticipantBijection> {
    if actual.len() != representatives.len() {
        return Vec::new();
    }
    permutations(representatives)
        .into_iter()
        .map(|permutation| {
            let result = ParticipantBijection::from_pairs(actual.iter().copied().zip(permutation));
            assert_eq!(result.len(), actual.len());
            result
        })
        .collect()
}

/// Function-summarization metadata owned by the model checker.
pub(crate) struct SummarizationRuntime {
    options: SummarizationOptions,

    #[cfg(test)]
    summary_hits: Cell<usize>,
    #[cfg(test)]
    summary_stores: usize,

    // Persistent across stateless executions.
    summaries: HashMap<SummarizableFunctionId, Vec<SummaryCase>>,

    // Reconstructed within each stateless execution.
    next_call_index: HashMap<SummarizableFunctionId, u64>,
    next_offer_occurrence: HashMap<(ThreadId, SummarizableFunctionId), u64>,
    call_states: HashMap<SummarizableCallId, SummarizableCallState>,
}

impl SummarizationRuntime {
    pub(crate) fn new(options: SummarizationOptions) -> Self {
        Self {
            options,
            #[cfg(test)]
            summary_hits: Cell::new(0),
            #[cfg(test)]
            summary_stores: 0,
            summaries: HashMap::new(),
            next_call_index: HashMap::new(),
            next_offer_occurrence: HashMap::new(),
            call_states: HashMap::new(),
        }
    }

    pub(crate) fn begin_execution(&mut self) {
        self.next_call_index.clear();
        self.next_offer_occurrence.clear();
        self.call_states.clear();
    }

    pub(crate) fn allocate_offer_id(
        &mut self,
        participant: ThreadId,
        function: SummarizableFunctionId,
    ) -> ParticipationOfferId {
        let occurrence = self
            .next_offer_occurrence
            .entry((participant, function.clone()))
            .or_insert(0);
        let id = ParticipationOfferId {
            function,
            participant,
            occurrence: *occurrence,
        };
        *occurrence += 1;
        id
    }

    pub(crate) fn validate_nested_specification(
        &self,
        parent: Option<&SummarizableCallId>,
        caller: ThreadId,
        specification: &Participants,
    ) {
        let Some(parent) = parent else {
            return;
        };
        let parent_state = self
            .call_states
            .get(parent)
            .expect("nested call refers to a missing parent call");
        assert!(parent_state.participants.contains(&caller));
        if let ParticipationMode::Join(resolver) = specification.mode {
            assert!(
                parent_state.participants.contains(&resolver),
                "a nested summarizable call cannot join a resolver outside its parent"
            );
        }
        for candidate in specification
            .required()
            .iter()
            .chain(specification.exactly_one().iter().flatten())
        {
            assert!(
                parent_state.participants.contains(candidate),
                "a nested summarizable call cannot add a participant outside its parent"
            );
        }
    }

    fn lookup_summary_outcomes(
        &self,
        function: &SummarizableFunctionId,
        participants: &[ThreadId],
        inputs: &ParticipantArguments,
    ) -> Option<Vec<SummaryOutcome>> {
        let cases = self.summaries.get(function)?;
        let result = cases
            .iter()
            .find_map(|case| case.instantiate_for(participants, inputs, self.options.symmetry()));
        #[cfg(test)]
        if result.is_some() {
            self.summary_hits.set(self.summary_hits.get() + 1);
        }
        result
    }

    fn contains_equivalent_case(
        &self,
        function: &SummarizableFunctionId,
        participants: &[ThreadId],
        inputs: &ParticipantArguments,
    ) -> bool {
        let Some(cases) = self.summaries.get(function) else {
            return false;
        };
        cases.iter().any(|case| {
            case.instantiate_for(participants, inputs, self.options.symmetry())
                .is_some()
        })
    }

    pub(crate) fn store_summary_case(
        &mut self,
        function: SummarizableFunctionId,
        participants: Vec<ThreadId>,
        inputs: ParticipantArguments,
        outcomes: Vec<SummaryOutcome>,
    ) {
        assert!(
            !self.contains_equivalent_case(&function, &participants, &inputs),
            "attempted to store an existing summary case twice"
        );
        let cases = self.summaries.entry(function).or_default();
        cases.push(SummaryCase {
            participants,
            inputs,
            outcomes,
        });
        #[cfg(test)]
        {
            self.summary_stores += 1;
        }
    }

    pub(crate) fn commit_group(
        &mut self,
        function: SummarizableFunctionId,
        offers: &[ParticipationOffer],
    ) -> SummarizableCallId {
        assert_group_is_closed(offers);
        assert!(offers.iter().all(|offer| offer.function == function));

        let call_index = self.next_call_index.entry(function.clone()).or_insert(0);
        let call = SummarizableCallId {
            function,
            call_index: *call_index,
        };
        *call_index += 1;

        let mut arguments_by_participant = BTreeMap::new();
        for offer in offers {
            let previous =
                arguments_by_participant.insert(offer.id.participant, offer.arguments.clone());
            assert!(previous.is_none(), "a group selected one thread twice");
        }
        let arguments = ParticipantValues::from_map(&arguments_by_participant);
        let participants = arguments
            .0
            .iter()
            .map(|(participant, _)| *participant)
            .collect::<Vec<_>>();
        let selector = *participants
            .first()
            .expect("cannot commit an empty participant vector");
        let previous = self.call_states.insert(
            call.clone(),
            SummarizableCallState::new(participants, arguments, selector),
        );
        assert!(previous.is_none(), "summarizable call id was reused");
        call
    }

    pub(crate) fn arrive_at_resolved_entry(
        &mut self,
        tid: ThreadId,
        call: SummarizableCallId,
        arguments: SummarizableArguments,
        expected_exploration: Option<(
            &SummarizableCallId,
            &[ThreadId],
            &ParticipantArguments,
            &ParticipantArguments,
        )>,
    ) -> (EntryAction, Vec<ThreadId>, Option<SummaryMiss>) {
        let call_state = self
            .call_states
            .get_mut(&call)
            .expect("grant refers to a missing summarizable call");
        assert!(
            call_state.participants.contains(&tid),
            "grant was delivered to a nonparticipant"
        );
        let expected_arguments = call_state
            .arguments
            .value_for(tid)
            .expect("resolved group has no arguments for participant");
        assert_eq!(
            expected_arguments, arguments,
            "a participant changed its arguments after posting its offer"
        );
        if !call_state.arrived_participants.contains(&tid) {
            call_state.arrived_participants.push(tid);
        }

        let entry_is_complete = call_state.mode.is_none()
            && call_state.arrived_participants.len() == call_state.participant_count();
        let mut miss = None;
        let mut wake = Vec::new();
        if entry_is_complete {
            let participants = call_state.participants.clone();
            let inputs = call_state.arguments.clone();
            let mut body_arguments = None;
            let mode = match expected_exploration {
                Some((
                    expected_call,
                    expected_participants,
                    expected_inputs,
                    expected_summary_inputs,
                )) if expected_call == &call => {
                    assert_eq!(expected_participants, participants);
                    assert_eq!(expected_inputs, &inputs);
                    body_arguments = Some(expected_summary_inputs.clone());
                    CallMode::ExploringBody
                }
                Some((expected_call, _, _, _)) => {
                    let outcomes = self
                        .lookup_summary_outcomes(&call.function, &participants, &inputs)
                        .unwrap_or_else(|| {
                            panic!(
                                "replay reached unsummarized call {:?} before active exploration {:?}",
                                call, expected_call
                            )
                        });
                    CallMode::ApplyingSummary {
                        outcomes,
                        selection: None,
                    }
                }
                None => {
                    if let Some(outcomes) =
                        self.lookup_summary_outcomes(&call.function, &participants, &inputs)
                    {
                        CallMode::ApplyingSummary {
                            outcomes,
                            selection: None,
                        }
                    } else {
                        let summary_inputs = inputs.abstract_inputs();
                        body_arguments = Some(summary_inputs.clone());
                        miss = Some(SummaryMiss {
                            participants,
                            actual_inputs: inputs,
                            summary_inputs,
                        });
                        CallMode::ExploringBody
                    }
                }
            };
            let call_state = self.call_states.get_mut(&call).unwrap();
            call_state.body_arguments = body_arguments;
            call_state.mode = Some(mode);
            wake.append(&mut call_state.waiting_participants);
        }

        let call_state = self.call_states.get_mut(&call).unwrap();
        let action = match call_state.mode.clone() {
            None => {
                if !call_state.waiting_participants.contains(&tid) {
                    call_state.waiting_participants.push(tid);
                }
                EntryAction::WaitForParticipants
            }
            Some(CallMode::ExploringBody) => EntryAction::ExecuteBody {
                arguments: call_state
                    .body_arguments
                    .as_ref()
                    .expect("summary body arguments were not initialized")
                    .value_for(tid)
                    .expect("summary body arguments are missing for participant"),
                handle: SummarizableCallHandle { call },
            },
            Some(CallMode::ApplyingSummary { selection, .. }) => {
                if selection.is_some() {
                    EntryAction::ApplySummary(SummarizableCallHandle { call })
                } else if tid == call_state.selector {
                    EntryAction::SelectSummaryOutcome {
                        handle: SummarizableCallHandle { call },
                    }
                } else {
                    if !call_state.waiting_participants.contains(&tid) {
                        call_state.waiting_participants.push(tid);
                    }
                    EntryAction::WaitForParticipants
                }
            }
        };
        (action, wake, miss)
    }

    pub(crate) fn summary_outcomes(&self, handle: &SummarizableCallHandle) -> Vec<SummaryOutcome> {
        let call_state = self
            .call_states
            .get(&handle.call)
            .expect("missing summarizable call state");
        let CallMode::ApplyingSummary { outcomes, .. } = call_state
            .mode
            .as_ref()
            .expect("summarizable entry barrier is incomplete")
        else {
            panic!("cannot inspect summary outcomes while exploring the body");
        };
        outcomes.clone()
    }

    pub(crate) fn replace_summary_outcomes(
        &mut self,
        handle: &SummarizableCallHandle,
        replacement: Vec<SummaryOutcome>,
    ) {
        let call_state = self
            .call_states
            .get_mut(&handle.call)
            .expect("missing summarizable call state");
        let CallMode::ApplyingSummary {
            outcomes,
            selection,
        } = call_state
            .mode
            .as_mut()
            .expect("summarizable entry barrier is incomplete")
        else {
            panic!("cannot replace summary outcomes while exploring the body");
        };
        assert!(selection.is_none(), "summary outcome was already selected");
        *outcomes = replacement;
    }

    pub(crate) fn selected_summary_outcome_template(
        &self,
        handle: &SummarizableCallHandle,
        index: usize,
    ) -> (Vec<ThreadId>, SummaryOutcome) {
        let call_state = self
            .call_states
            .get(&handle.call)
            .expect("missing summarizable call state");
        let CallMode::ApplyingSummary {
            outcomes,
            selection,
        } = call_state
            .mode
            .as_ref()
            .expect("summarizable entry barrier is incomplete")
        else {
            panic!("cannot select a summary outcome while exploring the body");
        };
        assert!(selection.is_none(), "summary outcome was already selected");
        let outcome = outcomes
            .get(index)
            .unwrap_or_else(|| panic!("summary outcome index {index} is out of bounds"))
            .clone();
        (call_state.participants.clone(), outcome)
    }

    pub(crate) fn commit_summary_selection(
        &mut self,
        handle: &SummarizableCallHandle,
        outcome: SummaryOutcome,
        choice: Event,
    ) -> Vec<ThreadId> {
        let call_state = self
            .call_states
            .get_mut(&handle.call)
            .expect("missing summarizable call state");
        let CallMode::ApplyingSummary { selection, .. } = call_state
            .mode
            .as_mut()
            .expect("summarizable entry barrier is incomplete")
        else {
            panic!("cannot commit a summary outcome while exploring the body");
        };
        assert!(selection.is_none(), "summary outcome was already selected");
        *selection = Some((outcome, choice));
        std::mem::take(&mut call_state.waiting_participants)
    }

    pub(crate) fn selected_summary_outcome(
        &self,
        handle: &SummarizableCallHandle,
    ) -> (SummaryOutcome, Event) {
        let call_state = self
            .call_states
            .get(&handle.call)
            .expect("missing summarizable call state");
        let CallMode::ApplyingSummary { selection, .. } = call_state
            .mode
            .as_ref()
            .expect("summarizable entry barrier is incomplete")
        else {
            panic!("the call is not applying a summary");
        };
        selection
            .clone()
            .expect("summary outcome has not been selected")
    }

    pub(crate) fn record_body_return(
        &mut self,
        tid: ThreadId,
        handle: &SummarizableCallHandle,
        value: ErasedSummarizableVal,
    ) -> bool {
        let call_state = self
            .call_states
            .get_mut(&handle.call)
            .expect("missing summarizable call state");
        assert!(call_state.participants.contains(&tid));
        let previous = call_state.returns_by_participant.insert(tid, value);
        assert!(
            previous.is_none(),
            "participant returned twice from one summarizable call"
        );
        call_state.returns_by_participant.len() == call_state.participant_count()
    }

    pub(crate) fn completed_body_returns(
        &self,
        call: &SummarizableCallId,
    ) -> Option<ParticipantReturns> {
        let call_state = self.call_states.get(call)?;
        (call_state.returns_by_participant.len() == call_state.participant_count())
            .then(|| ParticipantValues::from_map(&call_state.returns_by_participant))
    }
}

impl Default for SummarizationRuntime {
    fn default() -> Self {
        Self::new(SummarizationOptions::default())
    }
}

fn offer_domain_matches(offer: &ParticipationOffer, domain: &ParticipationDomain) -> bool {
    offer.domain() == *domain
}

enum ResolutionStep {
    Require(ThreadId),
    Choose(Vec<ThreadId>),
    Complete,
}

fn next_resolution_step(selected: &BTreeMap<ThreadId, ParticipationOffer>) -> ResolutionStep {
    for offer in selected.values() {
        for alternatives in offer.specification.exactly_one() {
            let selected_count = alternatives
                .iter()
                .filter(|candidate| selected.contains_key(candidate))
                .count();
            assert!(
                selected_count <= 1,
                "resolved group violates exactly_one_of"
            );
        }
    }

    for offer in selected.values() {
        for required in offer.specification.required() {
            if !selected.contains_key(required) {
                return ResolutionStep::Require(*required);
            }
        }
    }

    for offer in selected.values() {
        for alternatives in offer.specification.exactly_one() {
            if !alternatives
                .iter()
                .any(|candidate| selected.contains_key(candidate))
            {
                return ResolutionStep::Choose(alternatives.clone());
            }
        }
    }

    ResolutionStep::Complete
}

fn resolve_group(root: ParticipationOffer) -> Vec<ParticipationOffer> {
    assert!(
        root.specification.is_resolver(),
        "only an explicitly resolving invocation can resolve a group"
    );
    assert_eq!(root.id.participant, root.resolver);
    let domain = root.domain();
    let root_participant = root.id.participant;
    let mut selected = BTreeMap::from([(root_participant, root)]);

    loop {
        let offer = match next_resolution_step(&selected) {
            ResolutionStep::Require(required) => {
                let offer: ParticipationOffer = receive_internal_where(
                    SummarizationRendezvousLoc::Offers(domain.clone()),
                    move |sender| sender == required,
                );
                assert_eq!(
                    offer.id.participant, required,
                    "required offer payload does not match its sender"
                );
                offer
            }
            ResolutionStep::Choose(alternatives) => {
                let candidates = alternatives
                    .into_iter()
                    .filter(|candidate| !selected.contains_key(candidate))
                    .collect::<HashSet<_>>();
                assert!(
                    !candidates.is_empty(),
                    "an unresolved exactly_one_of clause has no candidates"
                );
                let accepted = candidates.clone();
                let offer: ParticipationOffer = receive_internal_where(
                    SummarizationRendezvousLoc::Offers(domain.clone()),
                    move |sender| accepted.contains(&sender),
                );
                assert!(
                    candidates.contains(&offer.id.participant),
                    "selected offer payload does not match its sender"
                );
                offer
            }
            ResolutionStep::Complete => break,
        };

        assert!(
            offer_domain_matches(&offer, &domain),
            "selected offer crosses function, depth, parent, or resolver boundary"
        );
        assert!(
            !offer.specification.is_resolver(),
            "a resolver cannot be selected as another resolver's participant"
        );
        let participant = offer.id.participant;
        assert!(
            selected.insert(participant, offer).is_none(),
            "a group selected one thread twice"
        );
    }

    let offers = selected.into_values().collect::<Vec<_>>();
    assert_group_is_closed(&offers);
    offers
}

fn assert_group_is_closed(offers: &[ParticipationOffer]) {
    let domain = offers
        .first()
        .expect("cannot validate an empty resolved group")
        .domain();
    assert!(
        offers
            .iter()
            .all(|offer| offer_domain_matches(offer, &domain)),
        "resolved group crosses function, depth, parent, or resolver boundary"
    );
    let participants = offers
        .iter()
        .map(|offer| offer.id.participant)
        .collect::<Vec<_>>();
    assert_eq!(
        participants.iter().collect::<HashSet<_>>().len(),
        participants.len(),
        "resolved group contains one participant more than once"
    );
    assert_eq!(
        offers
            .iter()
            .filter(|offer| offer.specification.is_resolver())
            .count(),
        1,
        "a resolved group must contain exactly one explicit resolver"
    );
    assert_eq!(
        offers
            .iter()
            .find(|offer| offer.specification.is_resolver())
            .unwrap()
            .id
            .participant,
        domain.resolver,
        "the resolving participant does not match the rendezvous domain"
    );
    for offer in offers {
        assert!(
            offer
                .specification
                .required()
                .iter()
                .all(|required| participants.contains(required)),
            "resolved group omits a required participant"
        );
        for alternatives in offer.specification.exactly_one() {
            assert_eq!(
                alternatives
                    .iter()
                    .filter(|candidate| participants.contains(candidate))
                    .count(),
                1,
                "resolved group violates exactly_one_of"
            );
        }
    }
}

fn expect_internal_message<T: 'static>(value: Val) -> T {
    let actual_type = value.type_name.clone();
    *value.as_any().downcast::<T>().unwrap_or_else(|_| {
        panic!(
            "internal summarization message type mismatch: expected {}, got {}",
            std::any::type_name::<T>(),
            actual_type
        )
    })
}

fn send_internal<T>(location: SummarizationRendezvousLoc, value: T)
where
    T: Message + 'static,
{
    switch();
    ExecutionState::with(|state| {
        let pos = state.next_pos();
        let location = Loc::new(location);
        let send = SendMsg::new(
            pos,
            SendLoc::new(&location, pos.thread, None, None),
            CommunicationModel::NoOrder,
            Val::new(value),
            MonitorSends::new(),
            false,
        );
        let wake = state.must.borrow_mut().handle_send(send);
        for event in wake {
            let Some(task_id) = state.must.borrow().to_task_id(event.thread) else {
                continue;
            };
            let task = state.get_mut(task_id);
            if task.is_stuck() && task.instructions as u32 == event.index - 1 {
                task.unstuck();
            }
        }
    });
}

fn receive_internal<T>(location: SummarizationRendezvousLoc) -> T
where
    T: Message + 'static,
{
    receive_internal_with_predicate(location, None)
}

fn receive_internal_where<T, F>(location: SummarizationRendezvousLoc, accepts: F) -> T
where
    T: Message + 'static,
    F: Fn(ThreadId) -> bool + Send + Sync + 'static,
{
    receive_internal_with_predicate(
        location,
        Some(PredicateType(Arc::new(move |sender, _tag| accepts(sender)))),
    )
}

fn receive_internal_with_predicate<T>(
    location: SummarizationRendezvousLoc,
    predicate: Option<PredicateType>,
) -> T
where
    T: Message + 'static,
{
    let location = Loc::new(location);
    loop {
        switch();
        let predicate = predicate.clone();
        let (value, _) = ExecutionState::with(|state| {
            let pos = state.next_pos();
            state.must.borrow_mut().handle_recv(
                RecvMsg::new(
                    pos,
                    RecvLoc::new(vec![&location], predicate, None),
                    CommunicationModel::NoOrder,
                    None,
                    false,
                ),
                true,
            )
        });
        match value {
            Some(value) if !value.is_pending() => return expect_internal_message(value),
            Some(_) => ExecutionState::with(|state| state.current_mut().stuck()),
            None => {}
        }
        ExecutionState::with(|state| {
            state.prev_pos();
        });
    }
}

fn wake_participants(state: &mut ExecutionState, tids: Vec<ThreadId>) {
    for tid in tids {
        let Some(task_id) = state.must.borrow().to_task_id(tid) else {
            continue;
        };
        if state.get(task_id).is_stuck() {
            state.get_mut(task_id).unstuck();
        }
    }
}

fn enter_resolved_call(
    call: SummarizableCallId,
    arguments: SummarizableArguments,
) -> SummaryDispatch {
    loop {
        switch();
        let action = ExecutionState::with(|state| {
            let tid = state.must.borrow().to_thread_id(state.current().id());
            let depth = state.current().summarizable_call_depth();
            let (action, wake) = state.must.borrow_mut().arrive_at_summarizable_entry(
                tid,
                depth,
                call.clone(),
                arguments.clone(),
            );
            wake_participants(state, wake);
            match &action {
                EntryAction::WaitForParticipants => state.current_mut().stuck(),
                EntryAction::ExecuteBody { handle, .. } | EntryAction::ApplySummary(handle) => {
                    state
                        .current_mut()
                        .enter_summarizable_call(handle.call.clone())
                }
                EntryAction::SelectSummaryOutcome { .. } => {}
            }
            action
        });
        match action {
            EntryAction::WaitForParticipants => continue,
            EntryAction::ExecuteBody { handle, arguments } => {
                return SummaryDispatch::ExecuteBody { handle, arguments };
            }
            EntryAction::ApplySummary(handle) => return SummaryDispatch::ApplySummary(handle),
            EntryAction::SelectSummaryOutcome { handle } => {
                switch();
                let selected = ExecutionState::with(|state| {
                    let consumer_owner = state.current().summarizable_call().cloned();
                    let outcome_count = state
                        .must
                        .borrow_mut()
                        .prepare_summary_outcomes(&handle, consumer_owner.as_ref());
                    if outcome_count == 0 {
                        let pos = state.next_pos();
                        state.must.borrow_mut().prune_summary_application(pos);
                        return None;
                    }

                    let pos = state.next_pos();
                    let mut range = 0..=(outcome_count - 1);
                    let selected = state
                        .must
                        .borrow_mut()
                        .handle_choice(Choice::new(pos, &mut range));
                    let (participants, outcome) = state
                        .must
                        .borrow()
                        .selected_summary_outcome_template(&handle, selected);
                    Some((participants, outcome, pos, consumer_owner.is_some()))
                });

                let Some((participants, outcome, choice, allow_parent_inputs)) = selected else {
                    continue;
                };
                let outcome = outcome.materialize(&participants, allow_parent_inputs);

                #[cfg(feature = "symbolic")]
                if let Some(guard) = outcome.guard() {
                    assume_selected_summary_guard(guard.clone());
                }

                if outcome.is_assumption_failed() {
                    ExecutionState::with(|state| {
                        let pos = state.next_pos();
                        state.must.borrow_mut().prune_summary_application(pos);
                    });
                    continue;
                }

                ExecutionState::with(|state| {
                    let wake = state
                        .must
                        .borrow_mut()
                        .commit_summary_selection(&handle, outcome, choice);
                    wake_participants(state, wake);
                });
            }
        }
    }
}

#[cfg(feature = "symbolic")]
fn assume_selected_summary_guard(expr: SymExpr) {
    ExecutionState::with(|state| {
        let pos = state.next_pos();
        let owner = state.current().summarizable_call().cloned();
        let label = ConstraintEval::new(pos, expr, true, ConstraintKind::FixedAssumption, owner);
        state.must.borrow_mut().handle_fixed_constraint(label);
    });
}

/// Enter a selectively-participated invocation. Used by generated code.
#[doc(hidden)]
pub fn __enter_with(
    descriptor: SummarizableFunctionDescriptor,
    specification: Participants,
    arguments: SummarizableArguments,
) -> SummaryDispatch {
    ExecutionState::with(|state| {
        state.must.borrow().validate_summarization_configuration();
    });
    let offer = ExecutionState::with(|state| {
        let tid = state.must.borrow().to_thread_id(state.current().id());
        let depth = state.current().summarizable_call_depth();
        let parent = state.current().summarizable_call().cloned();
        let mut specification = specification;
        specification.normalize_for(tid);
        let resolver = specification.resolver(tid);
        let id = {
            let mut must = state.must.borrow_mut();
            must.validate_nested_summarizable_specification(parent.as_ref(), tid, &specification);
            must.allocate_summarizable_offer(tid, descriptor.id())
        };
        let offer = ParticipationOffer {
            id: id.clone(),
            resolver,
            function: descriptor.id(),
            depth,
            parent,
            specification,
            arguments: arguments.clone(),
        };
        offer
    });

    if offer.specification.is_resolver() {
        let selected = resolve_group(offer.clone());
        let call = ExecutionState::with(|state| {
            state
                .must
                .borrow_mut()
                .commit_summarizable_group(offer.function.clone(), &selected)
        });
        for selected_offer in &selected {
            if selected_offer.id.participant == offer.id.participant {
                continue;
            }
            send_internal(
                SummarizationRendezvousLoc::Grant(selected_offer.id.clone()),
                ParticipationGrant {
                    offer: selected_offer.id.clone(),
                    call: call.clone(),
                },
            );
        }
        enter_resolved_call(call, arguments)
    } else {
        let domain = offer.domain();
        let reply_location = SummarizationRendezvousLoc::Grant(offer.id.clone());
        send_internal(SummarizationRendezvousLoc::Offers(domain), offer.clone());
        let grant = receive_internal::<ParticipationGrant>(reply_location);
        assert_eq!(grant.offer, offer.id);
        assert_eq!(grant.call.function, offer.function);
        enter_resolved_call(grant.call, arguments)
    }
}

/// Record one explored return. A summary-exploration run never returns to caller code.
#[doc(hidden)]
pub fn __complete_body(handle: SummarizableCallHandle, value: ErasedSummarizableVal) -> ! {
    ExecutionState::with(|state| {
        let tid = state.must.borrow().to_thread_id(state.current().id());
        state.current_mut().leave_summarizable_call(&handle.call);
        let all_participants_returned = state
            .must
            .borrow_mut()
            .record_body_return(tid, &handle, value);
        if !all_participants_returned {
            state.current_mut().stuck();
        }
    });
    loop {
        switch();
    }
}

/// Return this participant's projection of the selected global outcome.
#[doc(hidden)]
pub fn __apply_summary<T>(handle: SummarizableCallHandle) -> T
where
    T: Message + SummarizableVal + Send + 'static,
{
    let tid = ExecutionState::with(|state| state.must.borrow().to_thread_id(state.current().id()));
    let (outcome, _choice_event) =
        ExecutionState::with(|state| state.must.borrow().selected_summary_outcome(&handle));
    match outcome.result {
        SummaryResult::Returned(values) => {
            let value = values
                .value_for(tid)
                .unwrap_or_else(|| panic!("summary has no return value for thread {}", tid));
            ExecutionState::with(|state| {
                state.current_mut().leave_summarizable_call(&handle.call);
            });
            value.into_typed::<T>()
        }
        SummaryResult::Blocked => {
            ExecutionState::with(|state| {
                state.current_mut().leave_summarizable_call(&handle.call);
                let pos = state.next_pos();
                state
                    .must
                    .borrow_mut()
                    .block_on_summary_outcome(handle.call, pos);
            });
            loop {
                switch();
            }
        }
        SummaryResult::AssumptionFailed => {
            unreachable!("assumption failure is handled collectively by the selector")
        }
    }
}
