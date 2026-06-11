//! The OS-keychain seam (spec/RUNTIME.md §4.4): `api_key_ref` is a
//! keychain REFERENCE (`keychain:<service>/<account>`), resolved at call
//! time — config parsing already hard-errors on literal keys (P1.2, B13).
//!
//! P6.2 ships the seam + mock only; the platform resolvers (macOS
//! Keychain, Windows Credential Manager, Secret Service on Linux) are
//! founder-machine work behind this trait. Resolved keys never appear in
//! errors or logs ([`ConnectorError::Auth`] "never contains key material").

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{ConnectorError, ConnectorResult};

/// Resolve a `keychain:<service>/<account>` reference to key material at
/// call time. An empty `key_ref` means "no key" (`Ok(None)`).
pub trait KeyResolver: Send + Sync {
    fn resolve(&self, key_ref: &str) -> ConnectorResult<Option<String>>;
}

/// Parse a validated reference into `(service, account)`.
pub fn parse_key_ref(key_ref: &str) -> Option<(&str, &str)> {
    let rest = key_ref.strip_prefix("keychain:")?;
    let (service, account) = rest.split_once('/')?;
    (!service.is_empty() && !account.is_empty()).then_some((service, account))
}

/// Scriptable resolver for tests and for the P6.2 supervised wiring (no
/// cloud backend is live yet).
#[derive(Default)]
pub struct MockKeyResolver {
    entries: Mutex<HashMap<(String, String), String>>,
}

impl MockKeyResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, service: &str, account: &str, secret: &str) {
        self.entries
            .lock()
            .unwrap()
            .insert((service.to_owned(), account.to_owned()), secret.to_owned());
    }
}

impl KeyResolver for MockKeyResolver {
    fn resolve(&self, key_ref: &str) -> ConnectorResult<Option<String>> {
        if key_ref.is_empty() {
            return Ok(None);
        }
        let (service, account) = parse_key_ref(key_ref).ok_or_else(|| {
            ConnectorError::Auth(format!("unresolvable key reference shape: {key_ref}"))
        })?;
        match self
            .entries
            .lock()
            .unwrap()
            .get(&(service.to_owned(), account.to_owned()))
        {
            Some(secret) => Ok(Some(secret.clone())),
            // The error names the REFERENCE, never key material.
            None => Err(ConnectorError::Auth(format!(
                "no keychain entry for {key_ref}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ref_is_no_key_not_an_error() {
        let r = MockKeyResolver::new();
        assert!(matches!(r.resolve(""), Ok(None)));
    }

    #[test]
    fn resolves_known_entries_and_errors_without_leaking_material() {
        let r = MockKeyResolver::new();
        r.insert("photoproof", "openai-compat", "sk-SECRET");
        assert_eq!(
            r.resolve("keychain:photoproof/openai-compat").unwrap(),
            Some("sk-SECRET".into())
        );
        let err = r.resolve("keychain:photoproof/missing").unwrap_err();
        assert!(!err.to_string().contains("SECRET"));
    }

    #[test]
    fn parse_rejects_malformed_refs() {
        assert!(parse_key_ref("keychain:photoproof/acct").is_some());
        assert!(parse_key_ref("keychain:/acct").is_none());
        assert!(parse_key_ref("keychain:svc/").is_none());
        assert!(parse_key_ref("sk-literal").is_none());
    }
}
