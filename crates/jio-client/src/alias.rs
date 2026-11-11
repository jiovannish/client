use crate::{DEFAULT_REQUEST_TIMEOUT, SessionClient, decode, invalid, other, valid_session_id};
use serde::Deserialize;
use std::io;

/// Whether a name is a valid account VM alias, distinct from immutable VM IDs.
pub fn valid_vm_alias(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
        && !valid_session_id(name)
}

#[derive(Deserialize)]
struct Binding {
    session_id: String,
    alias: String,
}

impl SessionClient {
    /// Assigns or replaces this VM's account-wide alias. Requires hosted Server support.
    /// Existing names on other VMs are never overwritten.
    pub fn set_alias(&self, id: &str, name: &str) -> io::Result<()> {
        if !valid_session_id(id) || !valid_vm_alias(name) {
            return Err(invalid("expected a VM ID and a valid alias"));
        }
        let connection = self.connection()?;
        let binding: Binding = decode(
            connection
                .client
                .put(format!("{}/v1/sessions/{id}/alias", connection.base_url))
                .bearer_auth(&connection.api_key)
                .timeout(DEFAULT_REQUEST_TIMEOUT)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&serde_json::json!({"alias":name})).map_err(other)?)
                .send()
                .map_err(other)?,
        )?;
        if binding.session_id != id || binding.alias != name {
            return Err(invalid("endpoint returned an inconsistent alias binding"));
        }
        Ok(())
    }

    /// Resolves an account alias once, or returns an immutable ID without a request.
    /// Use the result for every step of an operation, including confirmations.
    pub fn resolve_session_id(&self, target: &str) -> io::Result<String> {
        if valid_session_id(target) {
            return Ok(target.to_owned());
        }
        if !valid_vm_alias(target) {
            return Err(invalid("invalid VM ID or alias"));
        }
        let connection = self.connection()?;
        let binding: Binding = decode(
            connection
                .client
                .get(format!("{}/v1/aliases/{target}", connection.base_url))
                .bearer_auth(&connection.api_key)
                .timeout(DEFAULT_REQUEST_TIMEOUT)
                .send()
                .map_err(other)?,
        )?;
        if !valid_session_id(&binding.session_id) || binding.alias != target {
            return Err(invalid("endpoint returned an inconsistent alias binding"));
        }
        Ok(binding.session_id)
    }
}

#[cfg(test)]
mod tests;
