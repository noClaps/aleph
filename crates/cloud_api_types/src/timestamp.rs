use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A timestamp with a serialized representation in RFC 3339 format.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct Timestamp(pub DateTime<Utc>);

impl Timestamp {
    pub fn new(datetime: DateTime<Utc>) -> Self {
        Self(datetime)
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(value: DateTime<Utc>) -> Self {
        Self(value)
    }
}

impl From<NaiveDateTime> for Timestamp {
    fn from(value: NaiveDateTime) -> Self {
        Self(value.and_utc())
    }
}

impl Serialize for Timestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let rfc3339_string = self.0.to_rfc3339_opts(SecondsFormat::Millis, true);
        serializer.serialize_str(&rfc3339_string)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let datetime = DateTime::parse_from_rfc3339(&value)
            .map_err(serde::de::Error::custom)?
            .to_utc();
        Ok(Self(datetime))
    }
}
