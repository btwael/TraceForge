use crate::{Envelope, Round};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PastRound<R: Round> {
    current: R,
    attempted: R,
}

impl<R: Round> PastRound<R> {
    pub(crate) fn new(current: R, attempted: R) -> Self {
        Self { current, attempted }
    }

    pub fn current(&self) -> &R {
        &self.current
    }

    pub fn attempted(&self) -> &R {
        &self.attempted
    }
}

impl<R: Round> std::fmt::Display for PastRound<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "round {:?} is from the past relative to current round {:?}",
            self.attempted, self.current
        )
    }
}

impl<R: Round> std::error::Error for PastRound<R> {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaleEnvelope<R: Round, M> {
    current: R,
    envelope: Envelope<R, M>,
}

impl<R: Round, M> StaleEnvelope<R, M> {
    #[allow(dead_code)]
    pub(crate) fn new(current: R, envelope: Envelope<R, M>) -> Self {
        Self { current, envelope }
    }

    pub fn current(&self) -> &R {
        &self.current
    }

    pub fn stamp(&self) -> &R {
        self.envelope.stamp()
    }

    pub fn envelope(&self) -> &Envelope<R, M> {
        &self.envelope
    }

    pub fn into_envelope(self) -> Envelope<R, M> {
        self.envelope
    }
}

impl<R: Round, M> std::fmt::Display for StaleEnvelope<R, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "envelope stamp {:?} is from the past relative to current round {:?}",
            self.envelope.stamp(),
            self.current
        )
    }
}

impl<R: Round, M: std::fmt::Debug> std::error::Error for StaleEnvelope<R, M> {}
