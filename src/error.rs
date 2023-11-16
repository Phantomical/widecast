use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecvError {
    /// The single sender has closed. No further messages will be sent.
    Closed,

    /// The receiver lagged too far behind and some messages have been skipped
    /// as a result. Attempting to receive again will return the oldest message
    /// still retained by the channel.
    ///
    /// Includes the number of skipped messages.
    Lagged(u64),
}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        TryRecvError::from(self.clone()).fmt(f)
    }
}

impl Error for RecvError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TryRecvError {
    /// The channel is currently empty.
    ///
    /// There is still an active [`Sender`] so data may become available at a
    /// later time.
    ///
    /// [`Sender`]: crate::Sender
    Empty,

    /// The single sender has closed. No further messages will be sent.
    Closed,

    /// The receiver lagged too far behind and some messages have been skipped
    /// as a result. Attempting to receive again will return the oldest message
    /// still retained by the channel.
    ///
    /// Includes the number of skipped messages.
    Lagged(u64),
}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("channel empty"),
            Self::Closed => f.write_str("channel closed"),
            Self::Lagged(amount) => write!(f, "channel lagged by {amount}"),
        }
    }
}

impl Error for TryRecvError {}

impl From<RecvError> for TryRecvError {
    fn from(value: RecvError) -> Self {
        match value {
            RecvError::Closed => Self::Closed,
            RecvError::Lagged(skipped) => Self::Lagged(skipped),
        }
    }
}
