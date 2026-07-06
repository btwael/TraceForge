#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetEnvelope<K, R, M> {
    key: K,
    stamp: R,
    msg: M,
}

impl<K, R, M> SetEnvelope<K, R, M> {
    pub fn new(key: K, stamp: R, msg: M) -> Self {
        Self { key, stamp, msg }
    }

    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn stamp(&self) -> &R {
        &self.stamp
    }

    pub fn into_parts(self) -> (K, R, M) {
        (self.key, self.stamp, self.msg)
    }

    pub(crate) fn msg(self) -> M {
        self.msg
    }
}
