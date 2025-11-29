//! Revisiting utilities

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::event::Event;
use crate::revisit::RevisitEnum::ForwardRevisit;
use std::fmt::Debug;

/// Models the different possible revisit types.  These all carry the
/// same info, but Must needs to be able to distinguish among them
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum RevisitEnum {
    ForwardRevisit(Revisit),
    BackwardRevisit(Revisit),
}

impl RevisitEnum {
    /// forward revisit of pos (recv or send) with new placement (send)
    pub(crate) fn new_forward(pos: Event, placement: Event) -> Self {
        RevisitEnum::ForwardRevisit(Revisit {
            pos,
            rev: RevisitPlacement::Default(placement),
        })
    }

    /// backward revisit of recv by send
    pub(crate) fn new_backward(recv: Event, send: Event) -> Self {
        RevisitEnum::BackwardRevisit(Revisit {
            pos: recv,
            rev: RevisitPlacement::Default(send),
        })
    }

    pub(crate) fn new_forward_inbox(pos: Event, placements: Vec<Event>) -> Self {
        RevisitEnum::ForwardRevisit(Revisit {
            pos,
            rev: RevisitPlacement::Inbox(placements),
        })
    }

    fn get_revisit(&self) -> &Revisit {
        match self {
            RevisitEnum::ForwardRevisit(r) => r,
            RevisitEnum::BackwardRevisit(r) => r,
        }
    }
    pub(crate) fn pos(&self) -> Event {
        self.get_revisit().pos
    }

    pub(crate) fn rev(&self) -> RevisitPlacement {
        self.get_revisit().rev.clone()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum RevisitPlacement {
    Default(Event),
    Inbox(Vec<Event>),
}

impl fmt::Display for RevisitPlacement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RevisitPlacement::Default(ev) => write!(f, "{}", ev),
            RevisitPlacement::Inbox(events) => {
                write!(f, "{{")?;
                for (i, ev) in events.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", ev)?;
                }
                write!(f, "}}")
            }
        }
    }
}

/// A revisit item to be examined by Must
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Revisit {
    /// the event whoce placement (rf or co choice) chages
    pub(crate) pos: Event,
    /// the placement (rf or co choice)
    pub(crate) rev: RevisitPlacement,
}

impl Revisit {
    pub(crate) fn new(pos: Event, rev: Event) -> Self {
        Self {
            pos,
            rev: RevisitPlacement::Default(rev),
        }
    }

    pub(crate) fn new_inbox(pos: Event, rev: Vec<Event>) -> Self {
        Self {
            pos,
            rev: RevisitPlacement::Inbox(rev),
        }
    }

    /// Return the event for a default revisit placement.
    /// Panics if called for an inbox placement.
    pub(crate) fn rev_event(&self) -> Event {
        match self.rev {
            RevisitPlacement::Default(ev) => ev,
            RevisitPlacement::Inbox(_) => panic!("expected default revisit placement"),
        }
    }

    /// Return the inbox placement events.
    /// Panics if called for a default placement.
    pub(crate) fn rev_inbox(&self) -> &Vec<Event> {
        match &self.rev {
            RevisitPlacement::Default(_) => panic!("expected inbox revisit placement"),
            RevisitPlacement::Inbox(v) => v,
        }
    }
}
