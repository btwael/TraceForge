use std::{fmt::Debug, hash::Hash};

pub trait KeyScheme: Clone + Eq + Hash + Debug + 'static {
    const LEN: usize;

    fn encode(&self, out: &mut Vec<u32>);

    fn decode(raw: &[u32]) -> Option<Self>;
}

impl KeyScheme for u32 {
    const LEN: usize = 1;

    fn encode(&self, out: &mut Vec<u32>) {
        out.push(*self);
    }

    fn decode(raw: &[u32]) -> Option<Self> {
        (raw.len() == Self::LEN).then_some(raw[0])
    }
}

impl KeyScheme for u64 {
    const LEN: usize = 2;

    fn encode(&self, out: &mut Vec<u32>) {
        out.push((self >> 32) as u32);
        out.push(*self as u32);
    }

    fn decode(raw: &[u32]) -> Option<Self> {
        if raw.len() != Self::LEN {
            return None;
        }
        Some(((raw[0] as u64) << 32) | raw[1] as u64)
    }
}
