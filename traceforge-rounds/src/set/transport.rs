use crate::Round;

use super::SetEnvelope;

pub trait SetTransport<K, R: Round, M> {
    type Node;
    type Error;

    fn send(&mut self, dst: Self::Node, envelope: SetEnvelope<K, R, M>) -> Result<(), Self::Error>;

    fn recv<F>(
        &mut self,
        key: &K,
        current: &R,
        filter: F,
    ) -> Result<Option<SetEnvelope<K, R, M>>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static;

    fn recv_block<F>(
        &mut self,
        key: &K,
        current: &R,
        filter: F,
    ) -> Result<SetEnvelope<K, R, M>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static;

    fn recv_keyed<F>(&mut self, filter: F) -> Result<Option<SetEnvelope<K, R, M>>, Self::Error>
    where
        F: Fn(&K, &R) -> bool + Send + Sync + 'static;

    fn inbox<F>(
        &mut self,
        key: &K,
        current: &R,
        filter: F,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<Option<SetEnvelope<K, R, M>>>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static;
}
