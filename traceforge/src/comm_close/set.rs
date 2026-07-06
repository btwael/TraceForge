use std::{any::type_name, sync::Arc};

use traceforge_rounds::{
    set::{KeyScheme, SetEnvelope, SetTransport},
    RoundScheme,
};

use super::{repeated_recv_count, TraceForgeTransportMode};
use crate::{msg::Message, thread::ThreadId};

pub type Set<K, R> = traceforge_rounds::set::SetComm<K, R, TraceForgeSetTransport>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TraceForgeSetTransport {
    mode: TraceForgeTransportMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceForgeSetTransportError {
    UnsupportedMode(TraceForgeTransportMode),
}

impl std::fmt::Display for TraceForgeSetTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedMode(mode) => {
                write!(
                    f,
                    "keyed comm_close does not support transport mode {mode:?}"
                )
            }
        }
    }
}

impl std::error::Error for TraceForgeSetTransportError {}

impl TraceForgeSetTransport {
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

pub fn set<K, R>() -> Set<K, R>
where
    K: KeyScheme,
    R: RoundScheme,
{
    set_with::<K, R>(TraceForgeTransportMode::default())
}

pub fn set_with<K, R>(mode: TraceForgeTransportMode) -> Set<K, R>
where
    K: KeyScheme,
    R: RoundScheme,
{
    traceforge_rounds::set::SetComm::new(TraceForgeSetTransport::with_mode(mode))
}

impl<K, R, M> SetTransport<K, R, M> for TraceForgeSetTransport
where
    K: KeyScheme + Send + Sync,
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
{
    type Node = ThreadId;
    type Error = TraceForgeSetTransportError;

    fn send(&mut self, dst: Self::Node, envelope: SetEnvelope<K, R, M>) -> Result<(), Self::Error> {
        match self.mode {
            TraceForgeTransportMode::TaggedNativeInbox
            | TraceForgeTransportMode::TaggedRepeatedRecv => {
                crate::send_vec_tagged_msg(
                    dst,
                    encode_set_tag(envelope.key(), envelope.stamp()),
                    envelope,
                );
                Ok(())
            }
            TraceForgeTransportMode::UntaggedRepeatedRecv => {
                Err(TraceForgeSetTransportError::UnsupportedMode(
                    TraceForgeTransportMode::UntaggedRepeatedRecv,
                ))
            }
        }
    }

    fn recv<F>(
        &mut self,
        key: &K,
        current: &R,
        filter: F,
    ) -> Result<Option<SetEnvelope<K, R, M>>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        match self.mode {
            TraceForgeTransportMode::TaggedNativeInbox
            | TraceForgeTransportMode::TaggedRepeatedRecv => {
                Ok(recv_set_tagged(key, current, filter))
            }
            TraceForgeTransportMode::UntaggedRepeatedRecv => {
                Err(TraceForgeSetTransportError::UnsupportedMode(
                    TraceForgeTransportMode::UntaggedRepeatedRecv,
                ))
            }
        }
    }

    fn recv_block<F>(
        &mut self,
        key: &K,
        current: &R,
        filter: F,
    ) -> Result<SetEnvelope<K, R, M>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        match self.mode {
            TraceForgeTransportMode::TaggedNativeInbox
            | TraceForgeTransportMode::TaggedRepeatedRecv => {
                Ok(recv_set_tagged_block(key, current, filter))
            }
            TraceForgeTransportMode::UntaggedRepeatedRecv => {
                Err(TraceForgeSetTransportError::UnsupportedMode(
                    TraceForgeTransportMode::UntaggedRepeatedRecv,
                ))
            }
        }
    }

    fn inbox<F>(
        &mut self,
        key: &K,
        current: &R,
        filter: F,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<Option<SetEnvelope<K, R, M>>>, Self::Error>
    where
        F: Fn(&R, &R) -> bool + Send + Sync + 'static,
    {
        match self.mode {
            TraceForgeTransportMode::TaggedNativeInbox => {
                Ok(inbox_set_tagged_native(key, current, filter, min, max))
            }
            TraceForgeTransportMode::TaggedRepeatedRecv => Ok(inbox_set_by_repeated_tagged_recv(
                key, current, filter, min, max,
            )),
            TraceForgeTransportMode::UntaggedRepeatedRecv => {
                Err(TraceForgeSetTransportError::UnsupportedMode(
                    TraceForgeTransportMode::UntaggedRepeatedRecv,
                ))
            }
        }
    }
}

fn recv_set_tagged<K, R, M, F>(key: &K, current: &R, filter: F) -> Option<SetEnvelope<K, R, M>>
where
    K: KeyScheme + Send + Sync,
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool + Send + Sync + 'static,
{
    let local_key = key.clone();
    let local_round = current.clone();
    crate::recv_vec_tagged_msg(move |_, tag| {
        let Some((remote_key, remote_round)) = decode_set_remote::<K, R>(tag) else {
            return false;
        };
        remote_key == local_key && filter(&local_round, &remote_round)
    })
}

fn recv_set_tagged_block<K, R, M, F>(key: &K, current: &R, filter: F) -> SetEnvelope<K, R, M>
where
    K: KeyScheme + Send + Sync,
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool + Send + Sync + 'static,
{
    let local_key = key.clone();
    let local_round = current.clone();
    crate::recv_vec_tagged_msg_block(move |_, tag| {
        let Some((remote_key, remote_round)) = decode_set_remote::<K, R>(tag) else {
            return false;
        };
        remote_key == local_key && filter(&local_round, &remote_round)
    })
}

fn inbox_set_tagged_native<K, R, M, F>(
    key: &K,
    current: &R,
    filter: F,
    min: usize,
    max: Option<usize>,
) -> Vec<Option<SetEnvelope<K, R, M>>>
where
    K: KeyScheme + Send + Sync,
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool + Send + Sync + 'static,
{
    let local_key = key.clone();
    let local_round = current.clone();
    let vals = crate::inbox_with_vec_tag_and_bounds(
        move |_, tag| {
            let Some((remote_key, remote_round)) = decode_set_remote::<K, R>(tag) else {
                return false;
            };
            remote_key == local_key && filter(&local_round, &remote_round)
        },
        min,
        max,
    );

    vals.into_iter()
        .map(decode_set_inbox_entry::<K, R, M>)
        .collect()
}

fn inbox_set_by_repeated_tagged_recv<K, R, M, F>(
    key: &K,
    current: &R,
    filter: F,
    min: usize,
    max: Option<usize>,
) -> Vec<Option<SetEnvelope<K, R, M>>>
where
    K: KeyScheme + Send + Sync,
    R: RoundScheme + Send + Sync,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
    F: Fn(&R, &R) -> bool + Send + Sync + 'static,
{
    let count = repeated_recv_count(min, max);
    let filter = Arc::new(filter);
    let mut messages = Vec::with_capacity(count);
    for _ in 0..count {
        let filter = Arc::clone(&filter);
        messages.push(Some(recv_set_tagged_block(key, current, {
            let local = current.clone();
            move |_, remote| filter(&local, remote)
        })));
    }
    messages
}

fn decode_set_inbox_entry<K, R, M>(entry: Option<crate::Val>) -> Option<SetEnvelope<K, R, M>>
where
    K: KeyScheme,
    R: RoundScheme,
    M: Message + Clone + PartialEq + std::fmt::Debug + 'static,
{
    let val = entry?;
    let actual_type_name = val.type_name.clone();
    let envelope = val
        .as_any()
        .downcast::<SetEnvelope<K, R, M>>()
        .unwrap_or_else(|_| {
            panic!(
                "wrong comm_close set inbox message return type; expecting {} but got {}",
                type_name::<SetEnvelope<K, R, M>>(),
                actual_type_name
            )
        });
    Some(*envelope)
}

fn decode_set_remote<K, R>(tag: Option<Vec<u32>>) -> Option<(K, R)>
where
    K: KeyScheme,
    R: RoundScheme,
{
    let raw = tag?;
    if raw.len() != K::LEN + R::LEN {
        return None;
    }
    let key = K::decode(&raw[..K::LEN])?;
    let round = R::decode(&raw[K::LEN..])?;
    Some((key, round))
}

fn encode_set_tag<K, R>(key: &K, round: &R) -> Vec<u32>
where
    K: KeyScheme,
    R: RoundScheme,
{
    let mut tag = Vec::with_capacity(K::LEN + R::LEN);
    key.encode(&mut tag);
    R::encode(round, &mut tag);
    tag
}
