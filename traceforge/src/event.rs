//! An event in an execution graph
use crate::thread::{main_thread_id, ThreadId};
use serde::{Deserialize, Serialize};
use crate::event_label::{Inbox, RecvMsg};

/// Models a single event in an execution graph.
#[derive(PartialEq, Copy, Clone, Debug, Hash, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Event {
    pub(crate) thread: ThreadId,
    pub(crate) index: u32,
}

impl Event {
    pub(crate) fn new(t: ThreadId, i: u32) -> Self {
        Self {
            thread: t,
            index: i,
        }
    }

    pub(crate) fn new_init() -> Self {
        Self::new(main_thread_id(), 0)
    }

    pub(crate) fn next(&self) -> Self {
        Self {
            thread: self.thread,
            index: self.index + 1,
        }
    }

    pub(crate) fn prev(&self) -> Self {
        Self {
            thread: self.thread,
            index: self.index - 1,
        }
    }
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.thread, self.index)
    }
}