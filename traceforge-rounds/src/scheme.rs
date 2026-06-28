pub trait RoundScheme: crate::Round {
    const LEN: usize;

    fn encode(round: &Self, out: &mut Vec<u32>);

    fn decode(raw: &[u32]) -> Option<Self>;
}
