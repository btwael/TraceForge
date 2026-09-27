//! Revisiting utilities

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::event::Event;
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
            trigger: None,
        })
    }

    /// backward revisit of recv by send
    pub(crate) fn new_backward(recv: Event, send: Event) -> Self {
        RevisitEnum::BackwardRevisit(Revisit::new(recv, send))
    }

    /// Forward revisit for an inbox event, replacing its chosen send set.
    pub(crate) fn new_forward_inbox(pos: Event, placements: Vec<Event>) -> Self {
        RevisitEnum::ForwardRevisit(Revisit {
            pos,
            rev: RevisitPlacement::Inbox(placements),
            trigger: None,
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

    pub(crate) fn rev(&self) -> &RevisitPlacement {
        &self.get_revisit().rev
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum RevisitPlacement {
    /// Classic revisit placement: single rf send.
    Default(Event),
    /// Inbox revisit placement: the whole (order-insensitive) chosen send set.
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
    /// The newly inserted send that initiated a backward revisit. This is
    /// distinct from the full set an inbox will read after that revisit.
    #[serde(default)]
    pub(crate) trigger: Option<Event>,
}

impl Revisit {
    pub(crate) fn new(pos: Event, rev: Event) -> Self {
        Self {
            pos,
            rev: RevisitPlacement::Default(rev),
            trigger: Some(rev),
        }
    }

    pub(crate) fn new_inbox(pos: Event, trigger: Event, rev: Vec<Event>) -> Self {
        assert!(rev.contains(&trigger));
        Self {
            pos,
            rev: RevisitPlacement::Inbox(rev),
            trigger: Some(trigger),
        }
    }

    pub(crate) fn trigger(&self) -> Event {
        self.trigger
            .or_else(|| match &self.rev {
                RevisitPlacement::Default(send) => Some(*send),
                RevisitPlacement::Inbox(_) => None,
            })
            .expect("backward inbox revisit must record its triggering send")
    }
}
