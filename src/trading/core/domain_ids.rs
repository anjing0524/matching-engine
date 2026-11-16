//! Domain identity newtypes.
//!
//! Phase 1 of the venue-model consolidation starts with sequence identity.
//! Keep this module small: each newtype replaces a primitive at one seam, then
//! the seam is migrated; do not add unused ids in advance.

/// Monotonic sequence in one event stream.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct EventSequence(u64);

impl EventSequence {
    /// Sequence zero, used by callers before an event is stamped.
    pub const ZERO: Self = Self(0);

    /// Creates a sequence value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw value for wire and snapshot boundaries.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the next sequence, saturating at `u64::MAX`.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns true when this sequence is after `other`.
    #[must_use]
    pub const fn is_after(self, other: Self) -> bool {
        self.0 > other.0
    }
}

impl From<u64> for EventSequence {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<EventSequence> for u64 {
    fn from(value: EventSequence) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::EventSequence;

    #[test]
    fn sequence_advances_and_compares() {
        let first = EventSequence::new(1);
        let second = first.next();
        assert_eq!(second.get(), 2);
        assert!(second.is_after(first));
        assert!(!first.is_after(second));
        assert_eq!(EventSequence::from(7).get(), 7);
        assert_eq!(u64::from(EventSequence::new(9)), 9);
    }

    #[test]
    fn sequence_saturates_at_max() {
        assert_eq!(EventSequence::new(u64::MAX).next().get(), u64::MAX);
    }
}

/// Exchange-assigned order id.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct OrderId(u64);

/// Client-assigned order id.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct ClientOrderId(u64);

/// Trading account id.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct AccountId(u64);

/// Gateway session id.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct SessionId(u64);

macro_rules! id_newtype {
    ($name:ident) => {
        impl $name {
            /// Creates the id from its wire representation.
            #[must_use]
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            /// Returns the wire representation.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl PartialEq<u64> for $name {
            fn eq(&self, other: &u64) -> bool {
                self.0 == *other
            }
        }
    };
}

id_newtype!(OrderId);
id_newtype!(ClientOrderId);
id_newtype!(AccountId);
id_newtype!(SessionId);
