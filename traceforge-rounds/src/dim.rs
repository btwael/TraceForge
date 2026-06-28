pub trait Dim: Copy + Eq + PartialOrd + std::fmt::Debug + 'static {
    fn initial() -> Self;

    fn next(self) -> Option<Self>;
}
