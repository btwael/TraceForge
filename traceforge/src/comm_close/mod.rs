use std::{any::type_name, convert::Infallible, sync::Arc};

use traceforge_rounds::{Envelope, RoundScheme, Transport};

use crate::{msg::Message, thread::ThreadId, Nondet};

pub type Comm<R> = traceforge_rounds::Comm<R, TraceForgeTransport>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TraceForgeTransportMode {
    #[default]
    TaggedNativeInbox,
    TaggedRepeatedRecv,
    UntaggedRepeatedRecv,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TraceForgeTransport {
    mode: TraceForgeTransportMode,
}

impl TraceForgeTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mode(mode: TraceForgeTransportMode) -> Self {
        Self { mode }
    }

    pub fn mode(&self) -> TraceForgeTransportMode {
        self.mode
    }
}

pub fn comm<R>() -> Comm<R>
where
    R: RoundScheme,
{
    comm_with::<R>(TraceForgeTransportMode::default())
}

pub fn comm_with<R>(mode: TraceForgeTransportMode) -> Comm<R>
where
    R: RoundScheme,
{
    traceforge_rounds::Comm::new(TraceForgeTransport::with_mode(mode))
}

impl<R, M> Transport<R, M> for TraceForgeTransport
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
{
    type Node = ThreadId;
    type Error = Infallible;

    fn send(&mut self, dst: Self::Node, envelope: Envelope<R, M>) -> Result<(), Self::Error> {
        match self.mode {
            TraceForgeTransportMode::TaggedNativeInbox
            | TraceForgeTransportMode::TaggedRepeatedRecv => {
                crate::send_vec_tagged_msg(dst, encode_tag(envelope.stamp()), envelope);
            }
            TraceForgeTransportMode::UntaggedRepeatedRecv => {
                crate::send_msg(dst, envelope);
            }
        }
        Ok(())
    }

    fn recv<F>(&mut self, current: &R, filter: F) -> Result<Option<Envelope<R, M>>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        Ok(match self.mode {
            TraceForgeTransportMode::TaggedNativeInbox
            | TraceForgeTransportMode::TaggedRepeatedRecv => recv_tagged(current, filter),
            TraceForgeTransportMode::UntaggedRepeatedRecv => recv_untagged(current, &filter),
        })
    }

    fn recv_block<F>(&mut self, current: &R, filter: F) -> Result<Envelope<R, M>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        Ok(match self.mode {
            TraceForgeTransportMode::TaggedNativeInbox
            | TraceForgeTransportMode::TaggedRepeatedRecv => recv_tagged_block(current, filter),
            TraceForgeTransportMode::UntaggedRepeatedRecv => recv_untagged_block(current, &filter),
        })
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
        Ok(match self.mode {
            TraceForgeTransportMode::TaggedNativeInbox => {
                inbox_tagged_native(current, filter, min, max)
            }
            TraceForgeTransportMode::TaggedRepeatedRecv => {
                inbox_by_repeated_tagged_recv(current, filter, min, max)
            }
            TraceForgeTransportMode::UntaggedRepeatedRecv => {
                inbox_by_repeated_untagged_recv(current, &filter, min, max)
            }
        })
    }
}

fn recv_tagged<R, M, F>(current: &R, filter: F) -> Option<Envelope<R, M>>
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool + Send + Sync + 'static,
{
    let local = current.clone();
    crate::recv_vec_tagged_msg(move |_, tag| {
        let Some(remote) = decode_remote::<R>(tag) else {
            return false;
        };
        filter(&local, &remote)
    })
}

fn recv_tagged_block<R, M, F>(current: &R, filter: F) -> Envelope<R, M>
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool + Send + Sync + 'static,
{
    let local = current.clone();
    crate::recv_vec_tagged_msg_block(move |_, tag| {
        let Some(remote) = decode_remote::<R>(tag) else {
            return false;
        };
        filter(&local, &remote)
    })
}

fn recv_untagged<R, M, F>(current: &R, filter: &F) -> Option<Envelope<R, M>>
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool,
{
    loop {
        let envelope = crate::recv_msg::<Envelope<R, M>>()?;
        if filter(current, envelope.stamp()) {
            return Some(envelope);
        }
    }
}

fn recv_untagged_block<R, M, F>(current: &R, filter: &F) -> Envelope<R, M>
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool,
{
    loop {
        let envelope = crate::recv_msg_block::<Envelope<R, M>>();
        if filter(current, envelope.stamp()) {
            return envelope;
        }
    }
}

fn inbox_tagged_native<R, M, F>(
    current: &R,
    filter: F,
    min: usize,
    max: Option<usize>,
) -> Vec<Option<Envelope<R, M>>>
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool + Send + Sync + 'static,
{
    let local = current.clone();
    let vals = crate::inbox_with_vec_tag_and_bounds(
        move |_, tag| {
            let Some(remote) = decode_remote::<R>(tag) else {
                return false;
            };
            filter(&local, &remote)
        },
        min,
        max,
    );

    vals.into_iter().map(decode_inbox_entry::<R, M>).collect()
}

fn inbox_by_repeated_tagged_recv<R, M, F>(
    current: &R,
    filter: F,
    min: usize,
    max: Option<usize>,
) -> Vec<Option<Envelope<R, M>>>
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool + Send + Sync + 'static,
{
    let count = repeated_recv_count(min, max);
    let filter = Arc::new(filter);
    let mut messages = Vec::with_capacity(count);
    for _ in 0..count {
        let filter = Arc::clone(&filter);
        messages.push(Some(recv_tagged_block(current, {
            let local = current.clone();
            move |_, remote| filter(&local, remote)
        })));
    }
    messages
}

fn inbox_by_repeated_untagged_recv<R, M, F>(
    current: &R,
    filter: &F,
    min: usize,
    max: Option<usize>,
) -> Vec<Option<Envelope<R, M>>>
where
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool,
{
    let count = repeated_recv_count(min, max);
    let mut messages = Vec::with_capacity(count);
    for _ in 0..count {
        messages.push(Some(recv_untagged_block(current, filter)));
    }
    messages
}

fn repeated_recv_count(min: usize, max: Option<usize>) -> usize {
    let Some(max) = max else {
        assert!(
            min > 0,
            "repeated-recv inbox requires max or a positive min"
        );
        return min;
    };
    assert!(max >= min, "inbox max must be greater than or equal to min");
    (min..=max).nondet()
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

fn decode_remote<R: RoundScheme>(tag: Option<Vec<u32>>) -> Option<R> {
    R::decode(&tag?)
}

fn encode_tag<R: RoundScheme>(round: &R) -> Vec<u32> {
    let mut tag = Vec::with_capacity(R::LEN);
    R::encode(round, &mut tag);
    tag
}
