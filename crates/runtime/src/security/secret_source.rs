//! The secret seam: how a host hands the kernel a credential by name, and what a credential store reports.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

/// A credential by name; `None` when the host holds no value. `Credentials` implements it.
pub trait SecretSource: Send + Sync {
    fn secret(&self, name: &str) -> Result<Option<String>, String>;
    /// Keep a derived secret (an OAuth token) until `ttl_ms` has passed.
    fn store_secret(&self, _name: &str, _value: &str, _ttl_ms: u64) -> Result<(), String> {
        Err("this host keeps no derived secrets".to_string())
    }
    /// The credential `name` of `principal` (a signed-in user's id), or the app-wide one when `None`.
    fn secret_of(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        match principal {
            None => self.secret(name),
            Some(_) => Err(NO_USER_CREDENTIALS.to_string()),
        }
    }
    /// The non-secret account label stored beside the credential `name` of `principal` (or the app-wide one), `None`
    /// when it has none or this host keeps none.
    fn label_of(&self, _name: &str, _principal: Option<&str>) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// [`Self::store_secret`] for `principal`'s credential.
    fn store_secret_of(
        &self,
        name: &str,
        value: &str,
        ttl_ms: u64,
        principal: Option<&str>,
    ) -> Result<(), String> {
        match principal {
            None => self.store_secret(name, value, ttl_ms),
            Some(_) => Err(NO_USER_CREDENTIALS.to_string()),
        }
    }
    /// Replace the credential `name` of `principal` (or the app-wide one when `None`) with `value`, durably: a
    /// rotated refresh token, unlike [`Self::store_secret_of`]'s expiring derived secret.
    fn rotate_secret_of(&self, _name: &str, _value: &str, _principal: Option<&str>) -> Result<(), String> {
        Err("this host keeps no rotatable credentials".to_string())
    }
    /// Drop the credential `name` of `principal` (or the app-wide one when `None`).
    fn forget_of(&self, _name: &str, _principal: Option<&str>) -> Result<(), String> {
        Err("this host keeps no credentials to forget".to_string())
    }
    /// Keep `value` as the credential `name` of `principal` (or the app-wide one when `None`) until it is forgotten:
    /// a durable upsert, unlike [`Self::store_secret_of`]'s expiring derived secret and [`Self::rotate_secret_of`]'s
    /// replacement of a credential that already exists.
    fn persist_secret_of(&self, _name: &str, _value: &str, _principal: Option<&str>) -> Result<(), String> {
        Err("this host keeps no durable credentials".to_string())
    }
    /// Whether [`Self::persist_secret_of`] keeps a value on this host.
    fn durable_credentials(&self) -> bool {
        false
    }
    /// The credential store's state (can it encrypt, which rows it holds), never a value; `None` when the host holds
    /// no store.
    fn credential_store_list(&self) -> Result<Option<crate::credential_admin::StoreListing>, String> {
        Ok(None)
    }
    /// The stored value of credential `name`, from the store only.
    fn credential_store_read(&self, _name: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Encrypt `value` into the store as credential `name` of `service`.
    fn credential_store_write(&self, _service: &str, _name: &str, _value: &str) -> Result<(), String> {
        Err(orbital_core_base::credential_admin::NO_STORE.to_string())
    }
    /// Delete the stored row of credential `name`; whether one existed.
    fn credential_store_remove(&self, _name: &str) -> Result<bool, String> {
        Err(orbital_core_base::credential_admin::NO_STORE.to_string())
    }
    /// Re-encrypt every stored row under the current master key.
    fn credential_store_rotate(&self) -> Result<crate::credential_admin::RotateCounts, String> {
        Err(orbital_core_base::credential_admin::NO_STORE.to_string())
    }
    /// The integrations mode flag (`mock` | `real`); `None` when the host keeps none.
    fn credential_mode(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Switch the integrations mode flag; the host may refuse. The mode in force afterwards.
    fn set_credential_mode(&self, _mode: &str) -> Result<String, String> {
        Err(orbital_core_base::credential_admin::NO_MANAGER.to_string())
    }
}

pub fn credential_status_connected(value: Option<&str>, principal: Option<&str>) -> bool {
    value.is_some_and(|value| principal.is_some() || !value.is_empty())
}

const NO_USER_CREDENTIALS: &str = "this host keeps no user-scoped credentials";
