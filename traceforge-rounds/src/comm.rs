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
        M: 'static,
    {
        self.recv_with(|_, _| true)
    }

    pub fn recv_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Option<M>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds.current();
        let received = self
            .transport
            .recv(current, move |local, remote| {
                is_not_past(remote, local) && filter(local, remote)
            })
            .map_err(CommError::Transport)?;

        match received {
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

    pub fn recv_stamped<M>(
        &mut self,
    ) -> Result<Option<(R, M)>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
    {
        self.recv_stamped_with(|_, _| true)
    }

    pub fn recv_stamped_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Option<(R, M)>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds.current();
        let received = self
            .transport
            .recv(current, move |local, remote| {
                is_not_past(remote, local) && filter(local, remote)
            })
            .map_err(CommError::Transport)?;

        match received {
            Some(envelope) if is_not_past(envelope.stamp(), self.rounds.current()) => {
                Ok(Some(envelope.into_parts()))
            }
            Some(envelope) => Err(CommError::Stale(StaleEnvelope::new(
                self.rounds.current().clone(),
                envelope,
            ))),
            None => Ok(None),
        }
    }

    pub fn recv_block<M>(&mut self) -> Result<M, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
    {
        self.recv_block_with(|_, _| true)
    }

    pub fn recv_block_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<M, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds.current();
        let envelope = self
            .transport
            .recv_block(current, move |local, remote| {
                is_not_past(remote, local) && filter(local, remote)
            })
            .map_err(CommError::Transport)?;

        if is_not_past(envelope.stamp(), self.rounds.current()) {
            Ok(envelope.msg())
        } else {
            Err(CommError::Stale(StaleEnvelope::new(
                self.rounds.current().clone(),
                envelope,
            )))
        }
    }

    pub fn recv_block_stamped<M>(
        &mut self,
    ) -> Result<(R, M), CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
    {
        self.recv_block_stamped_with(|_, _| true)
    }

    pub fn recv_block_stamped_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<(R, M), CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds.current();
        let envelope = self
            .transport
            .recv_block(current, move |local, remote| {
                is_not_past(remote, local) && filter(local, remote)
            })
            .map_err(CommError::Transport)?;

        if is_not_past(envelope.stamp(), self.rounds.current()) {
            Ok(envelope.into_parts())
        } else {
            Err(CommError::Stale(StaleEnvelope::new(
                self.rounds.current().clone(),
                envelope,
            )))
        }
    }

    pub fn inbox<M>(
        &mut self,
    ) -> Result<Vec<Option<M>>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
    {
        self.inbox_with_bounds_with(0, None, |_, _| true)
    }

    pub fn inbox_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Vec<Option<M>>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        self.inbox_with_bounds_with(0, None, filter)
    }

    pub fn inbox_with_bounds<M>(
        &mut self,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<Option<M>>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
    {
        self.inbox_with_bounds_with(min, max, |_, _| true)
    }

    pub fn inbox_with_bounds_with<M, F>(
        &mut self,
        min: usize,
        max: Option<usize>,
        filter: F,
    ) -> Result<Vec<Option<M>>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds.current();
        let received = self
            .transport
            .inbox(
                current,
                move |local, remote| is_not_past(remote, local) && filter(local, remote),
                min,
                max,
            )
            .map_err(CommError::Transport)?;

        let mut msgs = Vec::with_capacity(received.len());
        for entry in received {
            match entry {
                Some(envelope) if is_not_past(envelope.stamp(), self.rounds.current()) => {
                    msgs.push(Some(envelope.msg()));
                }
                Some(envelope) => {
                    return Err(CommError::Stale(StaleEnvelope::new(
                        self.rounds.current().clone(),
                        envelope,
                    )));
                }
                None => msgs.push(None),
            }
        }

        Ok(msgs)
    }

    pub fn inbox_stamped<M>(
        &mut self,
    ) -> Result<Vec<Option<(R, M)>>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
    {
        self.inbox_stamped_with_bounds_with(0, None, |_, _| true)
    }

    pub fn inbox_stamped_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Vec<Option<(R, M)>>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        self.inbox_stamped_with_bounds_with(0, None, filter)
    }

    pub fn inbox_stamped_with_bounds<M>(
        &mut self,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<Option<(R, M)>>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
    {
        self.inbox_stamped_with_bounds_with(min, max, |_, _| true)
    }

    pub fn inbox_stamped_with_bounds_with<M, F>(
        &mut self,
        min: usize,
        max: Option<usize>,
        filter: F,
    ) -> Result<Vec<Option<(R, M)>>, CommError<R, M, <T as Transport<R, M>>::Error>>
    where
        T: Transport<R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds.current();
        let received = self
            .transport
            .inbox(
                current,
                move |local, remote| is_not_past(remote, local) && filter(local, remote),
                min,
                max,
            )
            .map_err(CommError::Transport)?;

        let mut msgs = Vec::with_capacity(received.len());
        for entry in received {
            match entry {
                Some(envelope) if is_not_past(envelope.stamp(), self.rounds.current()) => {
                    msgs.push(Some(envelope.into_parts()));
                }
                Some(envelope) => {
                    return Err(CommError::Stale(StaleEnvelope::new(
                        self.rounds.current().clone(),
                        envelope,
                    )));
                }
                None => msgs.push(None),
            }
        }

        Ok(msgs)
    }
}
