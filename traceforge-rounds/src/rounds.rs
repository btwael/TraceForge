use crate::{round::is_not_past, PastRound, Round};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rounds<R: Round> {
    current: R,
}

impl<R: Round> Rounds<R> {
    pub fn new() -> Self {
        Self {
            current: R::initial(),
        }
    }

    pub fn current(&self) -> &R {
        &self.current
    }

    pub fn tick(&mut self) -> Option<&R> {
        let next = R::tick(&self.current)?;
        assert!(
            is_not_past(&next, &self.current),
            "tick moved round {:?} backward from {:?}",
            next,
            self.current
        );
        self.current = next;
        Some(&self.current)
    }

    pub fn advance(&mut self, dim: R::Dim) -> &R {
        let next = R::advance_dim(&self.current, dim);
        assert!(
            is_not_past(&next, &self.current),
            "advance({}) moved round {:?} backward from {:?}",
            R::dim_name(dim),
            next,
            self.current
        );
        self.current = next;
        &self.current
    }

    pub fn advance_to(&mut self, target: R) -> Result<&R, PastRound<R>> {
        self.jump(target)
    }

    pub fn jump(&mut self, target: R) -> Result<&R, PastRound<R>> {
        if !is_not_past(&target, &self.current) {
            return Err(PastRound::new(self.current.clone(), target));
        }

        self.current = target;
        Ok(&self.current)
    }
}

impl<R: Round> Default for Rounds<R> {
    fn default() -> Self {
        Self::new()
    }
}
