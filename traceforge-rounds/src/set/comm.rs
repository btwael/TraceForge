use std::{collections::HashMap, hash::Hash};

use crate::{round::is_not_past, Round, Rounds};

use super::{SetCommError, SetEnvelope, SetTransport, StaleSetEnvelope};

#[derive(Clone, Debug)]
pub struct SetComm<K, R: Round, T> {
    rounds: HashMap<K, Rounds<R>>,
    transport: T,
}

impl<K, R: Round, T> SetComm<K, R, T> {
    pub fn new(transport: T) -> Self {
        Self {
            rounds: HashMap::new(),
            transport,
        }
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
}

impl<K, R, T> SetComm<K, R, T>
where
    K: Clone + Eq + Hash,
    R: Round,
{
    pub fn on(&mut self, key: K) -> SetLane<'_, K, R, T> {
        self.rounds.entry(key.clone()).or_default();
        SetLane { parent: self, key }
    }

    fn rounds_for_mut(&mut self, key: &K) -> &mut Rounds<R> {
        self.rounds.entry(key.clone()).or_default()
    }
}

pub struct SetLane<'a, K, R: Round, T> {
    parent: &'a mut SetComm<K, R, T>,
    key: K,
}

impl<K, R, T> SetLane<'_, K, R, T>
where
    K: Clone + Eq + Hash,
    R: Round,
{
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn rounds(&mut self) -> &mut Rounds<R> {
        self.parent.rounds_for_mut(&self.key)
    }

    pub fn send<M>(
        &mut self,
        dst: <T as SetTransport<K, R, M>>::Node,
        msg: M,
    ) -> Result<(), <T as SetTransport<K, R, M>>::Error>
    where
        T: SetTransport<K, R, M>,
    {
        let stamp = self.rounds().current().clone();
        let envelope = SetEnvelope::new(self.key.clone(), stamp, msg);
        self.parent.transport.send(dst, envelope)
    }

    pub fn recv<M>(
        &mut self,
    ) -> Result<Option<M>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
    {
        self.recv_with(|_, _| true)
    }

    pub fn recv_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Option<M>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds().current().clone();
        let received = self
            .parent
            .transport
            .recv(&self.key, &current, move |local, remote| {
                is_not_past(remote, local) && filter(local, remote)
            })
            .map_err(SetCommError::Transport)?;

        match received {
            Some(envelope)
                if envelope.key() == &self.key
                    && is_not_past(envelope.stamp(), self.rounds().current()) =>
            {
                Ok(Some(envelope.msg()))
            }
            Some(envelope) => Err(SetCommError::Stale(StaleSetEnvelope::new(
                self.key.clone(),
                self.rounds().current().clone(),
                envelope,
            ))),
            None => Ok(None),
        }
    }

    pub fn recv_stamped<M>(
        &mut self,
    ) -> Result<Option<(R, M)>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
    {
        self.recv_stamped_with(|_, _| true)
    }

    pub fn recv_stamped_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Option<(R, M)>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds().current().clone();
        let received = self
            .parent
            .transport
            .recv(&self.key, &current, move |local, remote| {
                is_not_past(remote, local) && filter(local, remote)
            })
            .map_err(SetCommError::Transport)?;

        match received {
            Some(envelope)
                if envelope.key() == &self.key
                    && is_not_past(envelope.stamp(), self.rounds().current()) =>
            {
                let (_, stamp, msg) = envelope.into_parts();
                Ok(Some((stamp, msg)))
            }
            Some(envelope) => Err(SetCommError::Stale(StaleSetEnvelope::new(
                self.key.clone(),
                self.rounds().current().clone(),
                envelope,
            ))),
            None => Ok(None),
        }
    }

    pub fn recv_block<M>(
        &mut self,
    ) -> Result<M, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
    {
        self.recv_block_with(|_, _| true)
    }

    pub fn recv_block_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<M, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds().current().clone();
        let envelope = self
            .parent
            .transport
            .recv_block(&self.key, &current, move |local, remote| {
                is_not_past(remote, local) && filter(local, remote)
            })
            .map_err(SetCommError::Transport)?;

        if envelope.key() == &self.key && is_not_past(envelope.stamp(), self.rounds().current()) {
            Ok(envelope.msg())
        } else {
            Err(SetCommError::Stale(StaleSetEnvelope::new(
                self.key.clone(),
                self.rounds().current().clone(),
                envelope,
            )))
        }
    }

    pub fn recv_block_stamped<M>(
        &mut self,
    ) -> Result<(R, M), SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
    {
        self.recv_block_stamped_with(|_, _| true)
    }

    pub fn recv_block_stamped_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<(R, M), SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds().current().clone();
        let envelope = self
            .parent
            .transport
            .recv_block(&self.key, &current, move |local, remote| {
                is_not_past(remote, local) && filter(local, remote)
            })
            .map_err(SetCommError::Transport)?;

        if envelope.key() == &self.key && is_not_past(envelope.stamp(), self.rounds().current()) {
            let (_, stamp, msg) = envelope.into_parts();
            Ok((stamp, msg))
        } else {
            Err(SetCommError::Stale(StaleSetEnvelope::new(
                self.key.clone(),
                self.rounds().current().clone(),
                envelope,
            )))
        }
    }

    pub fn inbox<M>(
        &mut self,
    ) -> Result<Vec<Option<M>>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
    {
        self.inbox_with_bounds_with(0, None, |_, _| true)
    }

    pub fn inbox_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Vec<Option<M>>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        self.inbox_with_bounds_with(0, None, filter)
    }

    pub fn inbox_with_bounds<M>(
        &mut self,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<Option<M>>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
    {
        self.inbox_with_bounds_with(min, max, |_, _| true)
    }

    pub fn inbox_with_bounds_with<M, F>(
        &mut self,
        min: usize,
        max: Option<usize>,
        filter: F,
    ) -> Result<Vec<Option<M>>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds().current().clone();
        let received = self
            .parent
            .transport
            .inbox(
                &self.key,
                &current,
                move |local, remote| is_not_past(remote, local) && filter(local, remote),
                min,
                max,
            )
            .map_err(SetCommError::Transport)?;

        let mut msgs = Vec::with_capacity(received.len());
        for entry in received {
            match entry {
                Some(envelope)
                    if envelope.key() == &self.key
                        && is_not_past(envelope.stamp(), self.rounds().current()) =>
                {
                    msgs.push(Some(envelope.msg()));
                }
                Some(envelope) => {
                    return Err(SetCommError::Stale(StaleSetEnvelope::new(
                        self.key.clone(),
                        self.rounds().current().clone(),
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
    ) -> Result<Vec<Option<(R, M)>>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
    {
        self.inbox_stamped_with_bounds_with(0, None, |_, _| true)
    }

    pub fn inbox_stamped_with<M, F>(
        &mut self,
        filter: F,
    ) -> Result<Vec<Option<(R, M)>>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        self.inbox_stamped_with_bounds_with(0, None, filter)
    }

    pub fn inbox_stamped_with_bounds<M>(
        &mut self,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<Option<(R, M)>>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
    {
        self.inbox_stamped_with_bounds_with(min, max, |_, _| true)
    }

    pub fn inbox_stamped_with_bounds_with<M, F>(
        &mut self,
        min: usize,
        max: Option<usize>,
        filter: F,
    ) -> Result<Vec<Option<(R, M)>>, SetCommError<K, R, M, <T as SetTransport<K, R, M>>::Error>>
    where
        T: SetTransport<K, R, M>,
        M: 'static,
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let current = self.rounds().current().clone();
        let received = self
            .parent
            .transport
            .inbox(
                &self.key,
                &current,
                move |local, remote| is_not_past(remote, local) && filter(local, remote),
                min,
                max,
            )
            .map_err(SetCommError::Transport)?;

        let mut msgs = Vec::with_capacity(received.len());
        for entry in received {
            match entry {
                Some(envelope)
                    if envelope.key() == &self.key
                        && is_not_past(envelope.stamp(), self.rounds().current()) =>
                {
                    let (_, stamp, msg) = envelope.into_parts();
                    msgs.push(Some((stamp, msg)));
                }
                Some(envelope) => {
                    return Err(SetCommError::Stale(StaleSetEnvelope::new(
                        self.key.clone(),
                        self.rounds().current().clone(),
                        envelope,
                    )));
                }
                None => msgs.push(None),
            }
        }

        Ok(msgs)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, convert::Infallible};

    use crate::{Round, RoundScheme};

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    struct Slot(u32);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TestDim {
        Step,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TestRound {
        step: u32,
    }

    impl PartialOrd for TestRound {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.step.cmp(&other.step))
        }
    }

    unsafe impl crate::__private::TrustedRound for TestRound {}

    unsafe impl Round for TestRound {
        type Dim = TestDim;

        fn initial() -> Self {
            Self { step: 0 }
        }

        fn tick(current: &Self) -> Option<Self> {
            Some(Self {
                step: current.step.checked_add(1)?,
            })
        }

        fn advance_dim(current: &Self, _dim: Self::Dim) -> Self {
            Self {
                step: current.step + 1,
            }
        }

        fn dim_name(_dim: Self::Dim) -> &'static str {
            "step"
        }
    }

    impl RoundScheme for TestRound {
        const LEN: usize = 1;

        fn encode(round: &Self, out: &mut Vec<u32>) {
            out.push(round.step);
        }

        fn decode(raw: &[u32]) -> Option<Self> {
            (raw.len() == Self::LEN).then_some(Self { step: raw[0] })
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Msg {
        A,
        B,
    }

    #[derive(Default)]
    struct TestTransport {
        sent: Vec<(u32, SetEnvelope<Slot, TestRound, Msg>)>,
        queue: VecDeque<SetEnvelope<Slot, TestRound, Msg>>,
    }

    impl TestTransport {
        fn push(&mut self, envelope: SetEnvelope<Slot, TestRound, Msg>) {
            self.queue.push_back(envelope);
        }
    }

    impl SetTransport<Slot, TestRound, Msg> for TestTransport {
        type Node = u32;
        type Error = Infallible;

        fn send(
            &mut self,
            dst: Self::Node,
            envelope: SetEnvelope<Slot, TestRound, Msg>,
        ) -> Result<(), Self::Error> {
            self.sent.push((dst, envelope));
            Ok(())
        }

        fn recv<F>(
            &mut self,
            key: &Slot,
            current: &TestRound,
            filter: F,
        ) -> Result<Option<SetEnvelope<Slot, TestRound, Msg>>, Self::Error>
        where
            F: Fn(&TestRound, &TestRound) -> bool + Send + Sync + 'static,
        {
            let pos = self
                .queue
                .iter()
                .position(|entry| entry.key() == key && filter(current, entry.stamp()));
            Ok(pos.and_then(|pos| self.queue.remove(pos)))
        }

        fn recv_block<F>(
            &mut self,
            key: &Slot,
            current: &TestRound,
            filter: F,
        ) -> Result<SetEnvelope<Slot, TestRound, Msg>, Self::Error>
        where
            F: Fn(&TestRound, &TestRound) -> bool + Send + Sync + 'static,
        {
            Ok(self
                .recv(key, current, filter)?
                .expect("test transport has no matching message"))
        }

        fn inbox<F>(
            &mut self,
            key: &Slot,
            current: &TestRound,
            filter: F,
            min: usize,
            max: Option<usize>,
        ) -> Result<Vec<Option<SetEnvelope<Slot, TestRound, Msg>>>, Self::Error>
        where
            F: Fn(&TestRound, &TestRound) -> bool + Send + Sync + 'static,
        {
            let limit = max.unwrap_or(self.queue.len());
            let mut entries = Vec::new();
            while entries.len() < limit {
                let pos = self
                    .queue
                    .iter()
                    .position(|entry| entry.key() == key && filter(current, entry.stamp()));
                let Some(pos) = pos else {
                    break;
                };
                entries.push(self.queue.remove(pos));
            }

            assert!(
                entries.len() >= min,
                "test transport could not satisfy inbox lower bound"
            );

            if let Some(max) = max {
                while entries.len() < max {
                    entries.push(None);
                }
            }

            Ok(entries)
        }
    }

    #[test]
    fn rounds_are_independent_per_key() {
        let mut comm = SetComm::<Slot, TestRound, TestTransport>::new(TestTransport::default());

        comm.on(Slot(6)).rounds().advance(TestDim::Step);
        comm.on(Slot(6)).rounds().advance(TestDim::Step);
        comm.on(Slot(5)).send(9, Msg::A).unwrap();

        let (_, envelope) = &comm.transport().sent[0];
        assert_eq!(envelope.key(), &Slot(5));
        assert_eq!(envelope.stamp(), &TestRound { step: 0 });
    }

    #[test]
    fn receiving_on_one_key_uses_that_keys_local_round() {
        let mut comm = SetComm::<Slot, TestRound, TestTransport>::new(TestTransport::default());

        comm.on(Slot(5)).rounds().advance(TestDim::Step);
        comm.transport_mut()
            .push(SetEnvelope::new(Slot(6), TestRound { step: 0 }, Msg::B));

        let received = comm.on(Slot(6)).recv::<Msg>().unwrap();
        assert_eq!(received, Some(Msg::B));
    }

    #[test]
    fn inbox_is_scoped_to_the_selected_key() {
        let mut comm = SetComm::<Slot, TestRound, TestTransport>::new(TestTransport::default());
        comm.transport_mut()
            .push(SetEnvelope::new(Slot(5), TestRound { step: 0 }, Msg::A));
        comm.transport_mut()
            .push(SetEnvelope::new(Slot(6), TestRound { step: 0 }, Msg::B));

        let slot5 = comm
            .on(Slot(5))
            .inbox_with_bounds::<Msg>(0, Some(2))
            .unwrap();
        assert_eq!(slot5, vec![Some(Msg::A), None]);

        let slot6 = comm.on(Slot(6)).recv::<Msg>().unwrap();
        assert_eq!(slot6, Some(Msg::B));
    }
}
