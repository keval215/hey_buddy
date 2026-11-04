//! API key storage (ADR 0002 section 8). The key is read per request and lives only in a
//! sensitive header: never in `PreparedRequest`, the digest, `Debug`, serde, logs or config.

use std::sync::Arc;

use secrecy::SecretString;

use crate::LlmError;

pub const KEYCHAIN_SERVICE: &str = "hey_buddy";
pub const ANTHROPIC_ACCOUNT: &str = "anthropic";

pub trait CredentialStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<SecretString>, LlmError>;
    fn set(&self, account: &str, key: SecretString) -> Result<(), LlmError>;
    fn delete(&self, account: &str) -> Result<(), LlmError>;
}

/// Used wherever no OS keychain backend is wired (Linux in Step 1). `keyring` without a
/// backend silently falls back to an in-memory mock, so it is never called there.
pub struct UnsupportedCredentialStore;

impl CredentialStore for UnsupportedCredentialStore {
    fn get(&self, _account: &str) -> Result<Option<SecretString>, LlmError> {
        Ok(None)
    }
    fn set(&self, _account: &str, _key: SecretString) -> Result<(), LlmError> {
        Err(LlmError::Config("credential_store_unsupported"))
    }
    fn delete(&self, _account: &str) -> Result<(), LlmError> {
        Err(LlmError::Config("credential_store_unsupported"))
    }
}

#[cfg(all(feature = "keychain", any(target_os = "macos", target_os = "windows")))]
pub use keychain::KeyringStore;

#[cfg(all(feature = "keychain", any(target_os = "macos", target_os = "windows")))]
mod keychain {
    use secrecy::{ExposeSecret, SecretString};

    use super::{CredentialStore, KEYCHAIN_SERVICE};
    use crate::LlmError;

    /// macOS Keychain / Windows Credential Manager via `keyring`. Errors are mapped to fixed
    /// tokens; keyring's error text is never forwarded.
    pub struct KeyringStore;

    impl KeyringStore {
        fn entry(account: &str) -> Result<keyring::Entry, LlmError> {
            keyring::Entry::new(KEYCHAIN_SERVICE, account)
                .map_err(|_| LlmError::Config("keychain_error"))
        }
    }

    impl CredentialStore for KeyringStore {
        fn get(&self, account: &str) -> Result<Option<SecretString>, LlmError> {
            match Self::entry(account)?.get_password() {
                Ok(p) => Ok(Some(SecretString::from(p))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(LlmError::Config("keychain_error")),
            }
        }
        fn set(&self, account: &str, key: SecretString) -> Result<(), LlmError> {
            Self::entry(account)?
                .set_password(key.expose_secret())
                .map_err(|_| LlmError::Config("keychain_error"))
        }
        fn delete(&self, account: &str) -> Result<(), LlmError> {
            match Self::entry(account)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(LlmError::Config("keychain_error")),
            }
        }
    }
}

/// The platform's credential store: the OS keychain on macOS/Windows with the `keychain`
/// feature, otherwise [`UnsupportedCredentialStore`].
pub fn default_credential_store() -> Arc<dyn CredentialStore> {
    #[cfg(all(feature = "keychain", any(target_os = "macos", target_os = "windows")))]
    {
        Arc::new(KeyringStore)
    }
    #[cfg(not(all(feature = "keychain", any(target_os = "macos", target_os = "windows"))))]
    {
        Arc::new(UnsupportedCredentialStore)
    }
}
