use crate::runtime::thread::continuation::{ContinuationPool, PooledContinuation};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;
use crate::summarizable::SummarizableCallId;

// A note on terminology: we have competing notions of threads floating around. Here's the
// convention for disambiguating them:
// * A "thread" is a user-level unit of concurrency. User code creates threads, passes data
//   between them, etc.
// * A "future" is another user-level unit of concurrency, corresponding directly to Rust's notion
//   in std::future::Future. A future has a single method `poll` that can be used to resume
//   executing its computation. Both futures and threads are implemented in Task,
//   which wraps a continuation that is resumed when the task is scheduled.
// * A "task" is the Shuttle executor's reflection of a user-level unit of concurrency. Each task
//   has a corresponding continuation, which is the user-level code it runs, as well as a state like
//   "blocked", "runnable", etc. Scheduling algorithms take as input the state of all tasks
//   and decide which task should execute next. A context switch is when one task stops executing
//   and another begins.
// * A "continuation" is a low-level implementation of green threading for concurrency. Each
//   Task contains a corresponding continuation. When the Shuttle executor context switches to a
//   Task, the executor resumes that task's continuation until it yields, which happens when its
//   thread decides it might want to context switch (e.g., because it's blocked on a lock).

pub(crate) const DEFAULT_INLINE_TASKS: usize = 16;

/// A `Task` represents a user-level unit of concurrency. Each task has an `id` that is unique within
/// the execution, and a `state` reflecting whether the task is runnable (enabled) or not.
#[derive(Debug)]
pub(crate) struct Task {
    pub(super) id: TaskId,
    pub(super) state: TaskState,

    pub(super) continuation: Rc<RefCell<PooledContinuation>>,
    pub(crate) instructions: usize,
    name: Option<String>,

    // The active summarizable call provides communication closure for its messages.
    summarizable_call: Option<SummarizableCallId>,
}

impl Task {
    /// Create a task from a continuation
    fn new<F>(f: F, stack_size: usize, id: TaskId, name: Option<String>) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        let mut continuation = ContinuationPool::acquire(stack_size);
        continuation.initialize(Box::new(f));
        let continuation = Rc::new(RefCell::new(continuation));

        Self {
            id,
            state: TaskState::Runnable,
            continuation,
            instructions: 0,
            name,
            summarizable_call: None,
        }
    }

    pub(crate) fn from_closure<F>(
        f: F,
        stack_size: usize,
        id: TaskId,
        name: Option<String>,
        // clock: VectorClock,
    ) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self::new(f, stack_size, id, name)
    }

    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    pub(crate) fn runnable(&self) -> bool {
        self.state == TaskState::Runnable
    }

    pub(crate) fn is_stuck(&self) -> bool {
        self.state == TaskState::Stuck
    }

    pub(crate) fn finished(&self) -> bool {
        self.state == TaskState::Finished
    }

    pub(crate) fn stuck(&mut self) {
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Stuck;
    }

    pub(crate) fn unstuck(&mut self) {
        assert!(self.state == TaskState::Stuck);
        self.state = TaskState::Runnable;
    }

    pub(crate) fn finish(&mut self) {
        assert!(self.state != TaskState::Finished);
        self.state = TaskState::Finished;
    }

    pub(crate) fn name(&self) -> Option<String> {
        self.name.clone()
    }

    pub(crate) fn summarizable_call(&self) -> Option<&SummarizableCallId> {
        self.summarizable_call.as_ref()
    }

    pub(crate) fn enter_summarizable_call(&mut self, call: SummarizableCallId) {
        assert!(
            self.summarizable_call.is_none(),
            "nested summarizable function calls are not supported"
        );
        self.summarizable_call = Some(call);
    }

    pub(crate) fn leave_summarizable_call(&mut self) {
        assert!(
            self.summarizable_call.is_some(),
            "task left a summarizable function without entering one"
        );
        self.summarizable_call = None;
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum TaskState {
    /// Available to be scheduled
    Runnable,
    /// Waiting for the corresponding Send/End to actually be executed
    ///
    /// This is used during replay, when a receive or join wait for the corresponding
    /// dependency (that is already in the graph) to actually be executed.
    /// In that case, the receive/join event is added, the state is Stuck,
    /// and the instruction pointer points to the previous instruction.
    Stuck,
    /// Task has finished
    Finished,
}

/// A `TaskId` is a unique identifier for a task. `TaskId`s are never reused within a single
/// execution.
#[derive(PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct TaskId(pub(crate) usize);

impl From<usize> for TaskId {
    fn from(id: usize) -> Self {
        TaskId(id)
    }
}

impl From<TaskId> for usize {
    fn from(tid: TaskId) -> usize {
        tid.0
    }
}
