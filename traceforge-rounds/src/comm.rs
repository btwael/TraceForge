use crate::{round::is_not_past, Envelope, Rounds, StaleEnvelope, Transport};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommError<R: crate::Round, M, E> {
    Transport(E),
    Stale(StaleEnvelope<R, M>),
}

impl<R: crate::Round, M, E: std::fmt::Display> std::fmt::Display for CommError<R, M, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(err) => write!(f, "transport error: {err}"),
            Self::Stale(err) => err.fmt(f),
        }
    }
}

impl<R, M, E> std::error::Error for CommError<R, M, E>
where
    R: crate::Round,
    M: std::fmt::Debug,
    E: std::fmt::Debug + std::fmt::Display,
{
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Comm<R: crate::Round, T> {
    rounds: Rounds<R>,
    transport: T,
}

impl<R: crate::Round, T> Comm<R, T> {
    pub fn new(transport: T) -> Self {
        Self {
            rounds: Rounds::new(),
            transport,
        }
    }

    pub fn rounds(&mut self) -> &mut Rounds<R> {
        &mut self.rounds
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    pub fn send<M>(
        &mut self,
        dst: <T as Transport<R, M>>::Node,
        msg: M,
    ) -> Result<(), <T as Transport<R, M>>::Error>
    where
        T: Transport<R, M>,
    {
        let envelope = Envelope::new(self.rounds.current().clone(), msg);
        self.transport.send(dst, envelope)
    }

    pub fn recv<M>(&mut self) -> Result<Option<M>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
    {
        self.recv_with(|_, _| true)
    }

    pub fn recv_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Option<M>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        F: Fn(&R, &R) -> bool,
    {
        let current = self.rounds.current();
        let envelope = self
            .transport
            .recv(current, |stamp, current| {
                is_not_past(stamp, current) && filter(stamp, current)
            })
            .map_err(CommError::Transport)?;

        match envelope {
            Some(envelope) if is_not_past(envelope.stamp(), self.rounds.current()) => {
                Ok(Some(envelope.msg()))
            }
            Some(envelope) => Err(CommError::Stale(StaleEnvelope::new(
                self.rounds.current().clone(),
                envelope,
            ))),
            None => Ok(None),
        }
    }
}
