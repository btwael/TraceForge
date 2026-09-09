use crate::event::Event;
use crate::event_label::Choice;
use crate::msg::Message;
use crate::runtime::execution::ExecutionState;
use crate::runtime::thread::switch;
use crate::thread::ThreadId;
use crate::Val;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

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

/// Identity of one collective call to a summarizable function.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SummarizableCallId {
    pub function: SummarizableFunctionId,
    pub call_index: u64,
}

/// Static function information emitted by the macro `#[summarizable]`
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
    /// Execute the original function body while constructing its summary.
    ExecuteBody(SummarizableCallHandle),

    /// Apply an existing summary without executing the original body.
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

/// One exact participant-input vector and all distinct outcomes constructed for it.
#[derive(Clone, Debug)]
struct SummaryCase {
    inputs: ParticipantValues,
    outcomes: Vec<SummaryOutcome>,
}

/// How the current collective call is being evaluated.
#[derive(Clone, Debug)]
enum CallMode {
    /// Execute the original function body to construct its summary.
    ExploringBody,

    /// Skip the body and select one outcome from an existing summary.
    ApplyingSummary {
        outcomes: Vec<SummaryOutcome>,
        selection: Option<(usize, Event)>,
    },
}

/// Entry and return-barrier state for one collective call.
#[derive(Clone, Debug)]
struct SummarizableCallState {
    arguments_by_participant: BTreeMap<ThreadId, Val>,
    waiting_participants: Vec<ThreadId>,
    returns_by_participant: BTreeMap<ThreadId, Val>,
    mode: Option<CallMode>,
}

impl SummarizableCallState {
    fn new() -> Self {
        Self {
            arguments_by_participant: BTreeMap::new(),
            waiting_participants: Vec::new(),
            returns_by_participant: BTreeMap::new(),
            mode: None,
        }
    }
}

/// Action that the generated entry wrapper must perform.
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

/// Function-summarization metadata owned by the model checker.
pub(crate) struct SummarizationRuntime {
    // Persistent across stateless executions
    summaries: HashMap<SummarizableFunctionId, Vec<SummaryCase>>,
    expected_participants: Option<Vec<ThreadId>>,

    // Reconstructed within each stateless execution
    participants_sealed_this_execution: bool,
    next_call_index: HashMap<SummarizableFunctionId, u64>,
    active_call_by_participant: HashMap<ThreadId, SummarizableCallId>,
    call_states: HashMap<SummarizableCallId, SummarizableCallState>,

    // The collective currently collecting entry arrivals. This makes a
    // mismatch such as `A enters f` and `B enters g` fail immediately.
    assembling_call: Option<SummarizableCallId>,
}

impl SummarizationRuntime {
    pub(crate) fn new() -> Self {
        Self {
            summaries: HashMap::new(),
            expected_participants: None,
            participants_sealed_this_execution: false,
            next_call_index: HashMap::new(),
            active_call_by_participant: HashMap::new(),
            call_states: HashMap::new(),
            assembling_call: None,
        }
    }

    /// Reset per-execution call state while retaining constructed summaries.
    pub(crate) fn begin_execution(&mut self) {
        self.participants_sealed_this_execution = false;
        self.next_call_index.clear();
        self.active_call_by_participant.clear();
        self.call_states.clear();
        self.assembling_call = None;
    }

    pub(crate) fn seal_participants(&mut self, mut participants: Vec<ThreadId>) {
        participants.sort();
        participants.dedup();

        if let Some(expected) = &self.expected_participants {
            assert_eq!(
                expected, &participants,
                "summarization participant set changed between executions"
            );
        } else {
            self.expected_participants = Some(participants);
        }

        self.participants_sealed_this_execution = true;
    }

    pub(crate) fn participants_are_sealed(&self) -> bool {
        self.participants_sealed_this_execution
    }

    pub(crate) fn participant_count(&self) -> usize {
        self.expected_participants
            .as_ref()
            .expect(
                "call seal_summarization_participants() before entering a summarizable function",
            )
            .len()
    }

    pub(crate) fn outcome_selector(&self) -> ThreadId {
        self.expected_participants
            .as_ref()
            .and_then(|participants| participants.first().copied())
            .expect("summarization participant set is empty")
    }

    fn lookup_summary_outcomes(
        &self,
        function: &SummarizableFunctionId,
        inputs: &ParticipantValues,
    ) -> Option<Vec<SummaryOutcome>> {
        self.summaries
            .get(function)?
            .iter()
            .find(|entry| entry.inputs == *inputs)
            .map(|entry| entry.outcomes.clone())
    }

    pub(crate) fn store_summary_case(
        &mut self,
        function: SummarizableFunctionId,
        inputs: ParticipantValues,
        outcomes: Vec<SummaryOutcome>,
    ) {
        let cases = self.summaries.entry(function).or_default();

        assert!(
            !cases.iter().any(|case| case.inputs == inputs),
            "attempted to store an existing summary case twice"
        );

        cases.push(SummaryCase { inputs, outcomes });
    }

    fn assign_call(
        &mut self,
        tid: ThreadId,
        function: SummarizableFunctionId,
    ) -> SummarizableCallId {
        // __enter loops while a task waits. Repeated arrival by the same task
        // must reuse its previously assigned call.
        if let Some(call) = self.active_call_by_participant.get(&tid) {
            assert_eq!(
                call.function, function,
                "a participant changed summarizable function while waiting"
            );
            return call.clone();
        }

        let call = if let Some(assembling) = &self.assembling_call {
            assert_eq!(
                assembling.function, function,
                "summarization participants entered different collective functions"
            );
            assembling.clone()
        } else {
            let call_index = *self.next_call_index.entry(function.clone()).or_insert(0);

            let call = SummarizableCallId {
                function,
                call_index,
            };
            self.assembling_call = Some(call.clone());
            call
        };

        self.active_call_by_participant.insert(tid, call.clone());
        call
    }

    /// Record one participant at the entry barrier.
    pub(crate) fn arrive_at_entry(
        &mut self,
        tid: ThreadId,
        function: SummarizableFunctionId,
        arguments: Val,
        active_summary_key: Option<(&SummarizableFunctionId, &ParticipantValues)>,
    ) -> (EntryAction, Vec<ThreadId>, Option<ParticipantValues>) {
        assert!(
            self.participants_sealed_this_execution,
            "call seal_summarization_participants() before entering a summarizable function"
        );

        assert!(
            self.expected_participants
                .as_ref()
                .is_some_and(|participants| participants.contains(&tid)),
            "thread {tid} is not a sealed summarization participant"
        );

        let call = self.assign_call(tid, function);
        let participant_count = self.participant_count();
        let outcome_selector = self.outcome_selector();

        let call_state = self
            .call_states
            .entry(call.clone())
            .or_insert_with(SummarizableCallState::new);

        let previous_arguments = call_state
            .arguments_by_participant
            .entry(tid)
            .or_insert(arguments.clone())
            .clone();

        // __enter may call this repeatedly while the participant is waiting.
        assert_eq!(
            previous_arguments, arguments,
            "a participant changed its arguments while waiting at a summarizable entry"
        );

        let entry_is_complete = {
            let call_state = self.call_states.get(&call).unwrap();
            call_state.mode.is_none()
                && call_state.arguments_by_participant.len() == participant_count
        };

        let mut inputs_to_summarize = None;
        let mut wake = Vec::new();

        if entry_is_complete {
            let inputs = ParticipantValues::from_map(
                &self
                    .call_states
                    .get(&call)
                    .unwrap()
                    .arguments_by_participant,
            );

            // During summary exploration, every body revisit reruns the whole program and
            // reaches this same boundary again. It must continue the existing
            // exploration frame rather than create a second one.
            let continuing_summary_exploration =
                active_summary_key.is_some_and(|(active_function, active_inputs)| {
                    active_function == &call.function && active_inputs == &inputs
                });

            let mode = if continuing_summary_exploration {
                CallMode::ExploringBody
            } else if let Some(outcomes) = self.lookup_summary_outcomes(&call.function, &inputs) {
                CallMode::ApplyingSummary {
                    outcomes,
                    selection: None,
                }
            } else {
                inputs_to_summarize = Some(inputs);
                CallMode::ExploringBody
            };

            // The call index advances once, only after the collective has
            // assembled. Arrival order therefore cannot change it.
            *self
                .next_call_index
                .entry(call.function.clone())
                .or_insert(0) += 1;

            self.assembling_call = None;

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
                } else if tid == outcome_selector {
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

        (action, wake, inputs_to_summarize)
    }

    /// Select one outcome while applying an existing summary.
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

    /// Record one participant's return value from the explored body.
    pub(crate) fn record_body_return(
        &mut self,
        tid: ThreadId,
        handle: &SummarizableCallHandle,
        value: Val,
    ) -> bool {
        let participant_count = self.participant_count();
        let call_state = self
            .call_states
            .get_mut(&handle.call)
            .expect("missing summarizable call state");

        let previous = call_state.returns_by_participant.insert(tid, value);
        assert!(
            previous.is_none(),
            "participant returned twice from one summarizable call"
        );

        call_state.returns_by_participant.len() == participant_count
    }

    /// Return all participant values once every explored body has returned.
    pub(crate) fn completed_body_returns(
        &self,
        call: &SummarizableCallId,
    ) -> Option<ParticipantValues> {
        let call_state = self.call_states.get(call)?;
        (call_state.returns_by_participant.len() == self.participant_count())
            .then(|| ParticipantValues::from_map(&call_state.returns_by_participant))
    }

    pub(crate) fn finish_participant_call(&mut self, tid: ThreadId) {
        self.active_call_by_participant.remove(&tid);
    }
}

impl Default for SummarizationRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Wake the specified summarization participants.
fn wake_participants(state: &mut ExecutionState, tids: Vec<ThreadId>) {
    for tid in tids {
        let task_id = state.must.borrow().to_task_id(tid);
        let Some(task_id) = task_id else {
            continue;
        };

        if state.get(task_id).is_stuck() {
            state.get_mut(task_id).unstuck();
        }
    }
}

/// Freeze all currently live tasks as collective participants.
pub fn seal_summarization_participants() {
    ExecutionState::with(|state| {
        assert!(
            state.tasks.iter().all(|task| !task.finished()),
            "all summarization participants must be live when sealing"
        );

        let task_ids = state.tasks.iter().map(|task| task.id()).collect::<Vec<_>>();

        let participants = {
            let must = state.must.borrow();
            task_ids
                .into_iter()
                .map(|task_id| must.to_thread_id(task_id))
                .collect::<Vec<_>>()
        };

        state
            .must
            .borrow_mut()
            .seal_summarization_participants(participants);
    });
}

/// Enter the collective invocation (used by macro)
pub fn __enter(descriptor: SummarizableFunctionDescriptor, arguments: Val) -> SummaryDispatch {
    loop {
        switch();

        let action = ExecutionState::with(|state| {
            assert!(
                state.current().summarizable_call().is_none(),
                "nested summarizable function calls are not supported"
            );

            let tid = state.must.borrow().to_thread_id(state.current().id());

            let (action, wake) = state.must.borrow_mut().arrive_at_summarizable_entry(
                tid,
                descriptor.id(),
                arguments.clone(),
            );

            wake_participants(state, wake);

            match &action {
                EntryAction::WaitForParticipants => {
                    state.current_mut().stuck();
                }

                EntryAction::ExecuteBody(handle) => {
                    state
                        .current_mut()
                        .enter_summarizable_call(handle.call.clone());
                }

                EntryAction::SelectSummaryOutcome { .. } | EntryAction::ApplySummary(_) => {}
            }

            action
        });

        match action {
            EntryAction::WaitForParticipants => continue,

            EntryAction::ExecuteBody(handle) => {
                return SummaryDispatch::ExecuteBody(handle);
            }

            EntryAction::ApplySummary(handle) => {
                return SummaryDispatch::ApplySummary(handle);
            }

            EntryAction::SelectSummaryOutcome {
                handle,
                outcome_count,
            } => {
                assert!(outcome_count > 0, "a complete summary has no outcomes");

                switch();

                ExecutionState::with(|state| {
                    let pos = state.next_pos();
                    let mut range = 0..=(outcome_count - 1);

                    // handle_choice is already replay-aware and creates the
                    // forward revisits for the remaining outcome indexes.
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

/// Record one explored return. A summary-exploration run never returns to caller code.
pub fn __complete_body(handle: SummarizableCallHandle, value: Val) -> ! {
    ExecutionState::with(|state| {
        let tid = state.must.borrow().to_thread_id(state.current().id());

        state.current_mut().leave_summarizable_call();

        let all_participants_returned = state
            .must
            .borrow_mut()
            .record_body_return(tid, &handle, value);

        state.must.borrow_mut().finish_summarizable_call(tid);

        if !all_participants_returned {
            state.current_mut().stuck();
        }
    });

    loop {
        switch();
    }
}

/// Return this participant's projection of the selected global outcome.
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
                state.must.borrow_mut().finish_summarizable_call(tid);
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
                let pos = state.next_pos();
                let mut must = state.must.borrow_mut();
                must.finish_summarizable_call(tid);
                must.block_on_summary_outcome(handle.call, pos);
            });

            loop {
                switch();
            }
        }
    }
}

// TODO: #[doc(hidden)]
