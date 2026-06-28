pub unsafe trait Round:
    crate::__private::TrustedRound + Clone + PartialEq + PartialOrd + std::fmt::Debug + 'static
{
    type Dim: Copy + Eq + std::fmt::Debug + 'static;

    fn initial() -> Self;

    fn tick(current: &Self) -> Option<Self>;

    fn advance_dim(current: &Self, dim: Self::Dim) -> Self;

    fn dim_name(dim: Self::Dim) -> &'static str;
}

pub(crate) fn is_not_past<R: Round>(stamp: &R, current: &R) -> bool {
    matches!(
        stamp.partial_cmp(current),
        Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
    )
}
