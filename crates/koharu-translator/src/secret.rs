//! Credentials for one pipeline task.
//!
//! A caller hands a provider API key over with `PUT
//! /api/v1/config/providers/{id}/secret`. The server keeps that key in memory
//! until the task that consumes it ends: nothing here is written to disk and
//! nothing here reaches the OS credential service. [`ProviderSecrets`] is what
//! travels from the HTTP handler into the task subprocess and is dropped when
//! the task stops.
//!
//! The type is also the wire form of the task assignment frame, so it is
//! encoded with `revision` and deliberately does not implement serde traits.
//! `koharu_secrets::SecretString` cannot serve that role: its `Serialize` impl
//! always emits `"[REDACTED]"`, so a serde round trip would hand the child the
//! literal redaction text instead of the key.

use revision::revisioned;
use secrecy::zeroize::Zeroize;

use crate::Provider;

/// The provider credentials one task is allowed to use.
#[revisioned(revision = 1)]
#[derive(Clone, Default)]
pub struct ProviderSecrets {
    entries: Vec<Entry>,
}

#[revisioned(revision = 1)]
#[derive(Clone)]
struct Entry {
    provider: String,
    value: Value,
}

/// A key's bytes. Dropped zeroized so a finished task leaves no plaintext
/// behind on the heap. The `Drop` lives here rather than on
/// [`ProviderSecrets`] because `WorkerAssignment::into_request` has to move the
/// whole collection out of its parent struct, and a field cannot be moved out
/// of a type that implements `Drop`.
#[revisioned(revision = 1)]
#[derive(Clone)]
struct Value {
    bytes: Vec<u8>,
}

impl Drop for Value {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl ProviderSecrets {
    /// Records `value` as the credential for `provider`, replacing any earlier
    /// entry for the same provider.
    pub fn insert(&mut self, provider: Provider, value: String) {
        let id: &'static str = provider.into();
        self.entries.retain(|entry| entry.provider != id);
        self.entries.push(Entry {
            provider: id.to_owned(),
            value: Value {
                bytes: value.into_bytes(),
            },
        });
    }

    /// Drops the key held for `provider`. Dropping it here zeroes its bytes.
    pub fn remove(&mut self, provider: Provider) {
        let id: &'static str = provider.into();
        self.entries.retain(|entry| entry.provider != id);
    }

    /// The key this task holds for `provider`, or `None` when the caller sent
    /// none. This is the only way the bytes leave the collection.
    pub fn get(&self, provider: Provider) -> Option<&str> {
        let id: &'static str = provider.into();
        self.entries
            .iter()
            .find(|entry| entry.provider == id)
            .and_then(|entry| std::str::from_utf8(&entry.value.bytes).ok())
    }
}

#[cfg(test)]
mod tests {
    use revision::{from_slice, to_vec};

    use super::*;

    #[test]
    fn survives_a_revision_round_trip() {
        let mut secrets = ProviderSecrets::default();
        secrets.insert(Provider::DeepSeek, "sk-lab-canary-0001".to_owned());

        let encoded = to_vec(&secrets).expect("credentials encode");
        let decoded: ProviderSecrets = from_slice(&encoded).expect("credentials decode");

        assert_eq!(
            decoded.get(Provider::DeepSeek),
            Some("sk-lab-canary-0001"),
            "the revision codec must carry the key, not a redaction placeholder"
        );
    }

    #[test]
    fn insert_replaces_the_previous_key() {
        let mut secrets = ProviderSecrets::default();
        secrets.insert(Provider::DeepSeek, "first".to_owned());
        secrets.insert(Provider::DeepSeek, "second".to_owned());

        assert_eq!(secrets.get(Provider::DeepSeek), Some("second"));
    }

    #[test]
    fn providers_do_not_share_a_key() {
        let mut secrets = ProviderSecrets::default();
        secrets.insert(Provider::DeepSeek, "deepseek-key".to_owned());

        assert_eq!(secrets.get(Provider::OpenAiCompatible), None);
    }
}
