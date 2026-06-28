mod comm;
mod dim;
mod envelope;
mod error;
mod round;
mod rounds;
mod transport;

pub use comm::{Comm, CommError};
pub use dim::Dim;
pub use envelope::Envelope;
pub use error::{PastRound, StaleEnvelope};
pub use round::Round;
pub use rounds::Rounds;
pub use traceforge_rounds_macros::{Dim, Round};
pub use transport::Transport;

#[doc(hidden)]
pub mod __private {
    pub unsafe trait TrustedRound {}
}
