use std::{any::type_name, convert::Infallible};

use traceforge_rounds::{Envelope, RoundScheme, Transport};

use crate::{msg::Message, thread::ThreadId};

pub type Comm<R> = traceforge_rounds::Comm<R, TraceForgeTransport>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TraceForgeTransport;

impl TraceForgeTransport {
    pub fn new() -> Self {
        Self
    }
}

pub fn comm<R>() -> Comm<R>
where
    R: RoundScheme,
{
    traceforge_rounds::Comm::new(TraceForgeTransport)
}

impl<R, M> Transport<R, M> for TraceForgeTransport
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
{
    type Node = ThreadId;
    type Error = Infallible;

    fn send(&mut self, dst: Self::Node, envelope: Envelope<R, M>) -> Result<(), Self::Error> {
        crate::send_vec_tagged_msg(dst, encode_tag(envelope.stamp()), envelope);
        Ok(())
    }

    fn recv<F>(&mut self, current: &R, filter: F) -> Result<Option<Envelope<R, M>>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let local = current.clone();
        let received = crate::recv_vec_tagged_msg(move |_, tag| {
            let Some(tag) = tag else {
                return false;
            };
            let Some(remote) = R::decode(&tag) else {
                return false;
            };
            filter(&local, &remote)
        });

        Ok(received)
    }

    fn recv_block<F>(&mut self, current: &R, filter: F) -> Result<Envelope<R, M>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let local = current.clone();
        let received = crate::recv_vec_tagged_msg_block(move |_, tag| {
            let Some(tag) = tag else {
                return false;
            };
            let Some(remote) = R::decode(&tag) else {
                return false;
            };
            filter(&local, &remote)
        });

        Ok(received)
    }

    fn inbox<F>(
        &mut self,
        current: &R,
        filter: F,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<Option<Envelope<R, M>>>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        let local = current.clone();
        let vals = crate::inbox_with_vec_tag_and_bounds(
            move |_, tag| {
                let Some(tag) = tag else {
                    return false;
                };
                let Some(remote) = R::decode(&tag) else {
                    return false;
                };
                filter(&local, &remote)
            },
            min,
            max,
        );

        Ok(vals.into_iter().map(decode_inbox_entry::<R, M>).collect())
    }
}

fn decode_inbox_entry<R, M>(entry: Option<crate::Val>) -> Option<Envelope<R, M>>
where
    R: RoundScheme,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
{
    let val = entry?;
    let actual_type_name = val.type_name.clone();
    let envelope = val
        .as_any()
        .downcast::<Envelope<R, M>>()
        .unwrap_or_else(|_| {
            panic!(
                "wrong comm_close inbox message return type; expecting {} but got {}",
                type_name::<Envelope<R, M>>(),
                actual_type_name
            )
        });
    Some(*envelope)
}

fn encode_tag<R: RoundScheme>(round: &R) -> Vec<u32> {
    let mut tag = Vec::with_capacity(R::LEN);
    R::encode(round, &mut tag);
    tag
}
