use crate::{Envelope, Round};

pub trait Transport<R: Round, M> {
    type Node;
    type Error;

    fn send(&mut self, dst: Self::Node, envelope: Envelope<R, M>) -> Result<(), Self::Error>;

    fn recv<F>(&mut self, current: &R, filter: F) -> Result<Option<Envelope<R, M>>, Self::Error>
    where
        F: Fn(&R, &R) -> bool;
}
