//! Public package references and exact Nix flake source locks.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AttributePath, NarHash, NixpkgsRevision};

const MAX_LOCK_BYTES: usize = 256 * 1024;

/// A public GitHub flake and its package output, without arbitrary Nix options.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublicFlakeRef(String);

/// A public reference or its returned source lock failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlakeError;

impl fmt::Display for FlakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("public flake reference or lock is invalid")
    }
}

impl std::error::Error for FlakeError {}

impl PublicFlakeRef {
    /// Accepts `github:owner/repo[/ref][#package]` with bounded ASCII components.
    ///
    /// # Errors
    /// Returns [`FlakeError`] for an invalid reference, identity, or lock graph.
    pub fn new(value: &str) -> Result<Self, FlakeError> {
        if value.len() > 1024 {
            return Err(FlakeError);
        }
        let (source, attribute) = value.split_once('#').unwrap_or((value, "default"));
        AttributePath::new(attribute).map_err(|_| FlakeError)?;
        let parts = source.strip_prefix("github:").ok_or(FlakeError)?;
        let mut parts = parts.split('/');
        for component in [parts.next(), parts.next()] {
            let component = component.ok_or(FlakeError)?;
            if !safe_component(component) {
                return Err(FlakeError);
            }
        }
        if !parts.all(safe_component) {
            return Err(FlakeError);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the original user intent, including a branch or tag if supplied.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the fetch reference without the package fragment.
    #[must_use]
    pub fn source(&self) -> &str {
        self.0.split_once('#').map_or(self.0.as_str(), |(s, _)| s)
    }

    /// Returns the validated package fragment, defaulting to `default`.
    ///
    /// # Errors
    /// Returns [`FlakeError`] for an invalid reference, identity, or lock graph.
    pub fn attribute(&self) -> Result<AttributePath, FlakeError> {
        AttributePath::new(self.0.split_once('#').map_or("default", |(_, a)| a))
            .map_err(|_| FlakeError)
    }

    /// Returns the GitHub owner and repository, without a moving reference.
    #[must_use]
    pub fn repository(&self) -> &str {
        let source = &self.source()[7..];
        let end = source
            .match_indices('/')
            .nth(1)
            .map_or(source.len(), |(i, _)| i);
        &source[..end]
    }
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Exact root identity and the complete dependency graph used for evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LockedFlake {
    reference: PublicFlakeRef,
    revision: NixpkgsRevision,
    nar_hash: NarHash,
    locks: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LockWire {
    reference: String,
    revision: String,
    nar_hash: String,
    locks: String,
}

impl LockedFlake {
    /// Validates source identity and a complete, bounded, remote dependency graph.
    ///
    /// # Errors
    /// Returns [`FlakeError`] for an invalid reference, identity, or lock graph.
    pub fn new(
        reference: PublicFlakeRef,
        revision: NixpkgsRevision,
        nar_hash: NarHash,
        locks: &str,
    ) -> Result<Self, FlakeError> {
        if locks.len() > MAX_LOCK_BYTES {
            return Err(FlakeError);
        }
        let graph: Value = serde_json::from_str(locks).map_err(|_| FlakeError)?;
        validate_graph(&graph)?;
        let requested_revision = reference
            .source()
            .strip_prefix("github:")
            .and_then(|source| source.splitn(3, '/').nth(2));
        if let Some(requested) = requested_revision
            && let Ok(exact) = NixpkgsRevision::new(requested)
            && exact != revision
        {
            return Err(FlakeError);
        }
        Ok(Self {
            reference,
            revision,
            nar_hash,
            locks: graph.to_string(),
        })
    }

    /// Returns the user's repository, optional branch/tag, and package output.
    #[must_use]
    pub const fn reference(&self) -> &PublicFlakeRef {
        &self.reference
    }

    /// Returns the exact root Git commit.
    #[must_use]
    pub const fn revision(&self) -> &NixpkgsRevision {
        &self.revision
    }

    /// Returns the normalized root source hash.
    #[must_use]
    pub const fn nar_hash(&self) -> &NarHash {
        &self.nar_hash
    }

    /// Returns the exact dependency graph for Nix's reference lock file.
    #[must_use]
    pub fn locks_json(&self) -> &str {
        &self.locks
    }

    /// Encodes this validated source lock for durable state and plan hashing.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "reference": self.reference.as_str(),
            "revision": self.revision.as_str(),
            "narHash": self.nar_hash.as_str(),
            "locks": self.locks,
        })
        .to_string()
    }

    /// Decodes and validates a bounded saved source lock.
    ///
    /// # Errors
    /// Returns [`FlakeError`] for an invalid reference, identity, or lock graph.
    pub fn from_json(value: &str) -> Result<Self, FlakeError> {
        if value.len() > MAX_LOCK_BYTES * 2 + 2048 {
            return Err(FlakeError);
        }
        let wire: LockWire = serde_json::from_str(value).map_err(|_| FlakeError)?;
        Self::new(
            PublicFlakeRef::new(&wire.reference)?,
            NixpkgsRevision::new(&wire.revision).map_err(|_| FlakeError)?,
            NarHash::new(&wire.nar_hash).map_err(|_| FlakeError)?,
            &wire.locks,
        )
    }
}

fn validate_graph(graph: &Value) -> Result<(), FlakeError> {
    if graph.get("version").and_then(Value::as_u64) != Some(7) {
        return Err(FlakeError);
    }
    let root = graph
        .get("root")
        .and_then(Value::as_str)
        .ok_or(FlakeError)?;
    let nodes = graph
        .get("nodes")
        .and_then(Value::as_object)
        .ok_or(FlakeError)?;
    if nodes.is_empty() || nodes.len() > 512 || !nodes.contains_key(root) {
        return Err(FlakeError);
    }
    for (name, node) in nodes {
        let node = node.as_object().ok_or(FlakeError)?;
        if name == root {
            if node.contains_key("locked") {
                return Err(FlakeError);
            }
        } else {
            validate_locked_input(node.get("locked").ok_or(FlakeError)?)?;
        }
        if let Some(inputs) = node.get("inputs") {
            for input in inputs.as_object().ok_or(FlakeError)?.values() {
                match input {
                    Value::String(id) if nodes.contains_key(id) => {}
                    Value::Array(path)
                        if path.len() <= 64
                            && path
                                .iter()
                                .all(|part| part.as_str().is_some_and(safe_component)) => {}
                    _ => return Err(FlakeError),
                }
            }
        }
    }
    Ok(())
}

fn validate_locked_input(locked: &Value) -> Result<(), FlakeError> {
    NarHash::new(
        locked
            .get("narHash")
            .and_then(Value::as_str)
            .ok_or(FlakeError)?,
    )
    .map_err(|_| FlakeError)?;
    match locked.get("type").and_then(Value::as_str) {
        Some("github" | "gitlab") => {
            for key in ["owner", "repo"] {
                if !safe_component(locked.get(key).and_then(Value::as_str).ok_or(FlakeError)?) {
                    return Err(FlakeError);
                }
            }
            NixpkgsRevision::new(
                locked
                    .get("rev")
                    .and_then(Value::as_str)
                    .ok_or(FlakeError)?,
            )
            .map_err(|_| FlakeError)?;
            if locked.get("host").is_some() {
                return Err(FlakeError);
            }
        }
        Some("git" | "tarball") => {
            let url = locked
                .get("url")
                .and_then(Value::as_str)
                .ok_or(FlakeError)?;
            validate_https_url(url)?;
            if locked.get("type").and_then(Value::as_str) == Some("git") {
                NixpkgsRevision::new(
                    locked
                        .get("rev")
                        .and_then(Value::as_str)
                        .ok_or(FlakeError)?,
                )
                .map_err(|_| FlakeError)?;
            }
        }
        _ => return Err(FlakeError),
    }
    Ok(())
}

fn validate_https_url(url: &str) -> Result<(), FlakeError> {
    let rest = url.strip_prefix("https://").ok_or(FlakeError)?;
    if rest.split('/').next().is_none_or(str::is_empty)
        || rest.contains('@')
        || rest.contains('\\')
        || !rest.is_ascii()
        || rest
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err(FlakeError);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_references_and_locks_are_bounded_and_remote() {
        for value in [
            "github:owner/project",
            "github:owner/project/main#tool",
            "github:owner/project/feature/topic#packages.aarch64-darwin.tool",
        ] {
            let reference = PublicFlakeRef::new(value).unwrap();
            assert_eq!(reference.repository(), "owner/project");
        }
        for value in [
            "./local#tool",
            "path:/tmp/flake",
            "github:owner/repo?host=evil.test#tool",
            "github:owner/repo#x;bad",
            "github:owner/../repo",
            "github:owner/repo#",
            "github:owner/repo/#tool",
        ] {
            assert!(PublicFlakeRef::new(value).is_err(), "{value}");
        }
        let reference = PublicFlakeRef::new("github:owner/project#tool").unwrap();
        let revision = NixpkgsRevision::new(&"a".repeat(40)).unwrap();
        let hash = NarHash::new("sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap();
        let graph = r#"{"version":7,"root":"root","nodes":{"root":{}}}"#;
        let locked =
            LockedFlake::new(reference.clone(), revision.clone(), hash.clone(), graph).unwrap();
        assert_eq!(LockedFlake::from_json(&locked.to_json()).unwrap(), locked);
        for kind in ["path", "indirect", "file"] {
            let graph = serde_json::json!({"version":7,"root":"root","nodes":{"root":{},"bad":{"locked":{"type":kind,"path":"/etc","narHash":hash.as_str()}}}});
            assert!(
                LockedFlake::new(
                    reference.clone(),
                    revision.clone(),
                    hash.clone(),
                    &graph.to_string()
                )
                .is_err()
            );
        }
    }
}
