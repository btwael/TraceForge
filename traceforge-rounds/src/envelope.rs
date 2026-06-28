#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Envelope<R, M> {
    stamp: R,
    msg: M,
}

impl<R, M> Envelope<R, M> {
    #[allow(dead_code)]
    pub(crate) fn new(stamp: R, msg: M) -> Self {
        Self { stamp, msg }
    }

    pub fn stamp(&self) -> &R {
        &self.stamp
    }

    #[allow(dead_code)]
    pub(crate) fn msg(self) -> M {
        self.msg
    }
}
