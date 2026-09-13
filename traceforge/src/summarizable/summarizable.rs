use crate::event::Event;
use crate::event_label::{Choice, MonitorSends, RecvMsg, SendMsg};
use crate::loc::{CommunicationModel, Loc, RecvLoc, SendLoc};
use crate::msg::Message;
use crate::predicate::PredicateType;
use crate::runtime::execution::ExecutionState;
use crate::runtime::thread::switch;
use crate::thread::ThreadId;
use crate::Val;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

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

/// Identity of one thread's attempt to enter a summarizable function.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ParticipationOfferId {
    function: SummarizableFunctionId,
    participant: ThreadId,
    occurrence: u64,
}

/// Membership constraints supplied by one caller of a summarizable function.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Participants {
    required: Vec<ThreadId>,
    exactly_one: Vec<Vec<ThreadId>>,
}

impl Participants {
    /// Start with the calling thread as the only implicit participant.
    pub fn new() -> Self {
        Self::default()
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

    fn normalize_for(&mut self, caller: ThreadId) {
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

impl<const N: usize> From<[ThreadId; N]> for Participants {
    fn from(participants: [ThreadId; N]) -> Self {
        Self::new().require(participants)
    }
}

impl From<Vec<ThreadId>> for Participants {
    fn from(participants: Vec<ThreadId>) -> Self {
        Self::new().require(participants)
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
    ExecuteBody(SummarizableCallHandle),
    ApplySummary(SummarizableCallHandle),
}

/// A stable vector of dynamically typed values indexed by participant thread.
#[derive(Clone, Debug)]
pub(crate) struct ParticipantValues(pub(crate) Vec<(ThreadId, Val)>);

impl ParticipantValues {
    pub(crate) fn from_map(values: &BTreeMap<ThreadId, Val>) -> Self {
        Self(
            values
                .iter()
                .map(|(&tid, value)| (tid, value.clone()))
                .collect(),
        )
    }

    pub(crate) fn value_for(&self, tid: ThreadId) -> Option<Val> {
        self.0
            .iter()
            .find_map(|(candidate, value)| (*candidate == tid).then(|| value.clone()))
    }
}

impl PartialEq for ParticipantValues {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// One globally observable outcome of a summarizable function call.
#[derive(Clone, Debug)]
pub(crate) enum SummaryOutcome {
    Returned(ParticipantValues),
    Blocked,
}

impl PartialEq for SummaryOutcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Returned(left), Self::Returned(right)) => left == right,
            (Self::Blocked, Self::Blocked) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
struct SummaryCase {
    participants: Vec<ThreadId>,
    inputs: ParticipantValues,
    outcomes: Vec<SummaryOutcome>,
}

#[derive(Clone, Debug)]
enum CallMode {
    ExploringBody,
    ApplyingSummary {
        outcomes: Vec<SummaryOutcome>,
        selection: Option<(usize, Event)>,
    },
}

#[derive(Clone, Debug)]
struct SummarizableCallState {
    participants: Vec<ThreadId>,
    arguments: ParticipantValues,
    selector: ThreadId,
    arrived_participants: Vec<ThreadId>,
    waiting_participants: Vec<ThreadId>,
    returns_by_participant: BTreeMap<ThreadId, Val>,
    mode: Option<CallMode>,
}

impl SummarizableCallState {
    fn new(participants: Vec<ThreadId>, arguments: ParticipantValues, selector: ThreadId) -> Self {
        Self {
            participants,
            arguments,
            selector,
            arrived_participants: Vec::new(),
            waiting_participants: Vec::new(),
            returns_by_participant: BTreeMap::new(),
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
    ExecuteBody(SummarizableCallHandle),
    SelectSummaryOutcome {
        handle: SummarizableCallHandle,
        outcome_count: usize,
    },
    ApplySummary(SummarizableCallHandle),
}

#[derive(Clone, Debug)]
pub(crate) struct SummaryMiss {
    pub(crate) participants: Vec<ThreadId>,
    pub(crate) inputs: ParticipantValues,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipationOffer {
    id: ParticipationOfferId,
    function: SummarizableFunctionId,
    depth: usize,
    parent: Option<SummarizableCallId>,
    specification: Participants,
    arguments: Val,
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
}

impl ParticipationOffer {
    fn domain(&self) -> ParticipationDomain {
        ParticipationDomain {
            function: self.function.clone(),
            depth: self.depth,
            parent: self.parent.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SummarizationRendezvousLoc {
    Offers(ParticipationDomain),
    Grant(ParticipationOfferId),
}

/// Function-summarization metadata owned by the model checker.
pub(crate) struct SummarizationRuntime {
    // Persistent across stateless executions.
    summaries: HashMap<SummarizableFunctionId, Vec<SummaryCase>>,

    // Reconstructed within each stateless execution.
    next_call_index: HashMap<SummarizableFunctionId, u64>,
    next_offer_occurrence: HashMap<(ThreadId, SummarizableFunctionId), u64>,
    call_states: HashMap<SummarizableCallId, SummarizableCallState>,
}

impl SummarizationRuntime {
    pub(crate) fn new() -> Self {
        Self {
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
        inputs: &ParticipantValues,
    ) -> Option<Vec<SummaryOutcome>> {
        self.summaries
            .get(function)?
            .iter()
            .find(|case| case.participants == participants && case.inputs == *inputs)
            .map(|case| case.outcomes.clone())
    }

    pub(crate) fn store_summary_case(
        &mut self,
        function: SummarizableFunctionId,
        participants: Vec<ThreadId>,
        inputs: ParticipantValues,
        outcomes: Vec<SummaryOutcome>,
    ) {
        let cases = self.summaries.entry(function).or_default();
        assert!(
            !cases
                .iter()
                .any(|case| case.participants == participants && case.inputs == inputs),
            "attempted to store an existing summary case twice"
        );
        cases.push(SummaryCase {
            participants,
            inputs,
            outcomes,
        });
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
        arguments: Val,
        expected_exploration: Option<(&SummarizableCallId, &[ThreadId], &ParticipantValues)>,
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
            let mode = match expected_exploration {
                Some((expected_call, expected_participants, expected_inputs))
                    if expected_call == &call =>
                {
                    assert_eq!(expected_participants, participants);
                    assert_eq!(expected_inputs, &inputs);
                    CallMode::ExploringBody
                }
                Some((expected_call, _, _)) => {
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
                        miss = Some(SummaryMiss {
                            participants,
                            inputs,
                        });
                        CallMode::ExploringBody
                    }
                }
            };
            let call_state = self.call_states.get_mut(&call).unwrap();
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
            Some(CallMode::ExploringBody) => {
                EntryAction::ExecuteBody(SummarizableCallHandle { call })
            }
            Some(CallMode::ApplyingSummary {
                outcomes,
                selection,
            }) => {
                if selection.is_some() {
                    EntryAction::ApplySummary(SummarizableCallHandle { call })
                } else if tid == call_state.selector {
                    EntryAction::SelectSummaryOutcome {
                        handle: SummarizableCallHandle { call },
                        outcome_count: outcomes.len(),
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

    pub(crate) fn select_summary_outcome(
        &mut self,
        handle: &SummarizableCallHandle,
        index: usize,
        choice: Event,
    ) -> Vec<ThreadId> {
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
            panic!("cannot select a summary outcome while exploring the body");
        };
        assert!(index < outcomes.len());
        *selection = Some((index, choice));
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
        let CallMode::ApplyingSummary {
            outcomes,
            selection,
        } = call_state
            .mode
            .as_ref()
            .expect("summarizable entry barrier is incomplete")
        else {
            panic!("the call is not applying a summary");
        };
        let (index, choice) = selection.expect("summary outcome has not been selected");
        (outcomes[index].clone(), choice)
    }

    pub(crate) fn record_body_return(
        &mut self,
        tid: ThreadId,
        handle: &SummarizableCallHandle,
        value: Val,
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
    ) -> Option<ParticipantValues> {
        let call_state = self.call_states.get(call)?;
        (call_state.returns_by_participant.len() == call_state.participant_count())
            .then(|| ParticipantValues::from_map(&call_state.returns_by_participant))
    }
}

impl Default for SummarizationRuntime {
    fn default() -> Self {
        Self::new()
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

fn resolves_own_group(offer: &ParticipationOffer) -> bool {
    // A choice is resolved by the participant that declared it. A fixed group
    // elects the smallest locally declared participant so replay never depends
    // on which participant happened to reach the boundary last.
    if !offer.specification.exactly_one().is_empty() || offer.specification.required().is_empty() {
        return true;
    }

    offer
        .specification
        .required()
        .iter()
        .copied()
        .chain(std::iter::once(offer.id.participant))
        .min()
        == Some(offer.id.participant)
}

fn resolve_group(root: ParticipationOffer) -> Vec<ParticipationOffer> {
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
            "selected offer crosses function, depth, or parent boundary"
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
        "resolved group crosses function, depth, or parent boundary"
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
    if offers
        .iter()
        .all(|offer| offer.specification.exactly_one().is_empty())
    {
        assert_eq!(
            offers
                .iter()
                .filter(|offer| resolves_own_group(offer))
                .count(),
            1,
            "fixed participants disagree about the summarizable-call resolver"
        );
    }
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

fn enter_resolved_call(call: SummarizableCallId, arguments: Val) -> SummaryDispatch {
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
                EntryAction::ExecuteBody(handle) | EntryAction::ApplySummary(handle) => state
                    .current_mut()
                    .enter_summarizable_call(handle.call.clone()),
                EntryAction::SelectSummaryOutcome { .. } => {}
            }
            action
        });
        match action {
            EntryAction::WaitForParticipants => continue,
            EntryAction::ExecuteBody(handle) => return SummaryDispatch::ExecuteBody(handle),
            EntryAction::ApplySummary(handle) => return SummaryDispatch::ApplySummary(handle),
            EntryAction::SelectSummaryOutcome {
                handle,
                outcome_count,
            } => {
                assert!(outcome_count > 0, "a complete summary has no outcomes");
                switch();
                ExecutionState::with(|state| {
                    let pos = state.next_pos();
                    let mut range = 0..=(outcome_count - 1);
                    let selected = state
                        .must
                        .borrow_mut()
                        .handle_choice(Choice::new(pos, &mut range));
                    let wake = state
                        .must
                        .borrow_mut()
                        .select_summary_outcome(&handle, selected, pos);
                    wake_participants(state, wake);
                });
            }
        }
    }
}

/// Enter a selectively-participated invocation. Used by generated code.
#[doc(hidden)]
pub fn __enter_with(
    descriptor: SummarizableFunctionDescriptor,
    specification: impl Into<Participants>,
    arguments: Val,
) -> SummaryDispatch {
    ExecutionState::with(|state| {
        state.must.borrow().validate_summarization_configuration();
    });
    let offer = ExecutionState::with(|state| {
        let tid = state.must.borrow().to_thread_id(state.current().id());
        let depth = state.current().summarizable_call_depth();
        let parent = state.current().summarizable_call().cloned();
        let mut specification = specification.into();
        specification.normalize_for(tid);
        let id = {
            let mut must = state.must.borrow_mut();
            must.validate_nested_summarizable_specification(parent.as_ref(), tid, &specification);
            must.allocate_summarizable_offer(tid, descriptor.id())
        };
        let offer = ParticipationOffer {
            id: id.clone(),
            function: descriptor.id(),
            depth,
            parent,
            specification,
            arguments: arguments.clone(),
        };
        offer
    });

    if resolves_own_group(&offer) {
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
pub fn __complete_body(handle: SummarizableCallHandle, value: Val) -> ! {
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
    T: Message + 'static,
{
    let tid = ExecutionState::with(|state| state.must.borrow().to_thread_id(state.current().id()));
    let (outcome, _choice_event) =
        ExecutionState::with(|state| state.must.borrow().selected_summary_outcome(&handle));
    match outcome {
        SummaryOutcome::Returned(values) => {
            let value = values
                .value_for(tid)
                .unwrap_or_else(|| panic!("summary has no return value for thread {}", tid));
            ExecutionState::with(|state| {
                state.current_mut().leave_summarizable_call(&handle.call);
            });
            let actual_type = value.type_name.clone();
            *value.as_any().downcast::<T>().unwrap_or_else(|_| {
                panic!(
                    "summary return type mismatch: expected {}, got {}",
                    std::any::type_name::<T>(),
                    actual_type
                )
            })
        }
        SummaryOutcome::Blocked => {
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
    }
}
