pub trait Dim: Copy + Eq + PartialOrd + std::fmt::Debug + 'static {
    fn initial() -> Self;

    fn next(self) -> Option<Self>;

    fn to_index(self) -> u32;

    fn from_index(index: u32) -> Option<Self>;
}
