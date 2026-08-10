//! Append-free signing audit artifact written into a new publication.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

/// Allowlisted audit event for one signing operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent<'a> {
    /// Audit schema version.
    pub schema_version: u64,
    /// Reviewed release identifier.
    pub release_id: &'a str,
    /// Canonical release-manifest digest.
    pub release_digest: &'a str,
    /// CI/OIDC actor or service identity.
    pub actor: &'a str,
    /// Public online signing key ids, sorted by caller policy.
    pub key_ids: &'a [String],
    /// RFC3339 signing time supplied by the trusted workflow clock.
    pub signed_at: &'a str,
}

/// Creates a single-event private audit log; an existing path is refused.
pub fn write_audit_log(path: &Path, event: &AuditEvent<'_>) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer(&mut file, event)?;
    file.write_all(b"\n")?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{AuditEvent, write_audit_log};

    #[test]
    fn audit_log_is_allowlisted_newline_terminated_and_create_only() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("signing.ndjson");
        let keys = vec!["public-key-id".to_owned()];
        let event = AuditEvent {
            schema_version: 1,
            release_id: "v1.0.0",
            release_digest: "digest",
            actor: "release-service",
            key_ids: &keys,
            signed_at: "2026-08-10T00:00:00Z",
        };
        write_audit_log(&path, &event).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert!(!String::from_utf8_lossy(&bytes).contains("private"));
        assert!(write_audit_log(&path, &event).is_err());
    }
}
