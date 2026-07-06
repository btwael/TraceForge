use crate::Round;

use super::SetEnvelope;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetCommError<K, R: Round, M, E> {
    Transport(E),
    Stale(StaleSetEnvelope<K, R, M>),
}

impl<K, R, M, E> std::fmt::Display for SetCommError<K, R, M, E>
where
    K: std::fmt::Debug,
    R: Round,
    E: std::fmt::Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(err) => write!(f, "transport error: {err}"),
            Self::Stale(err) => err.fmt(f),
        }
    }
}

impl<K, R, M, E> std::error::Error for SetCommError<K, R, M, E>
where
    K: std::fmt::Debug,
    R: Round,
    M: std::fmt::Debug,
    E: std::fmt::Debug + std::fmt::Display,
{
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaleSetEnvelope<K, R: Round, M> {
    key: K,
    current: R,
    envelope: SetEnvelope<K, R, M>,
}

impl<K, R: Round, M> StaleSetEnvelope<K, R, M> {
    pub(crate) fn new(key: K, current: R, envelope: SetEnvelope<K, R, M>) -> Self {
        Self {
            key,
            current,
            envelope,
        }
    }

    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn current(&self) -> &R {
        &self.current
    }

    pub fn stamp(&self) -> &R {
        self.envelope.stamp()
    }

    pub fn envelope(&self) -> &SetEnvelope<K, R, M> {
        &self.envelope
    }

    pub fn into_envelope(self) -> SetEnvelope<K, R, M> {
        self.envelope
    }
}

impl<K, R, M> std::fmt::Display for StaleSetEnvelope<K, R, M>
where
    K: std::fmt::Debug,
    R: Round,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "envelope for key {:?} has stamp {:?}, which is from the past relative to current round {:?}",
            self.key,
            self.envelope.stamp(),
            self.current
        )
    }
}

impl<K, R, M> std::error::Error for StaleSetEnvelope<K, R, M>
where
    K: std::fmt::Debug,
    R: Round,
    M: std::fmt::Debug,
{
}
