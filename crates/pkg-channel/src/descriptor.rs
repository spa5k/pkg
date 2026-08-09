use std::collections::BTreeMap;

use pkg_core::{ChannelName, NarHash, NixpkgsRevision};
use serde::Deserialize;

/// Local-build behavior authenticated by the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildMode {
    /// Substitution first, then a gated native build after explicit approval.
    AllowWithGates,
    /// Approval is required, without asserting all V1 security gates.
    Prompt,
    /// Substitution only.
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireSystemEntry {
    pub url: String,
    pub sha256: String,
    #[serde(rename = "assetManifestTarget")]
    pub asset_manifest_target: String,
    #[serde(rename = "assetManifestSha256")]
    pub asset_manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireIndexEntry {
    pub target: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireNixRuntime {
    pub version: String,
    #[serde(rename = "perSystem")]
    pub per_system: BTreeMap<String, WireSystemEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireNixpkgs {
    pub owner: String,
    pub repo: String,
    pub rev: String,
    #[serde(rename = "narHash")]
    pub nar_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireIndex {
    pub source: String,
    #[serde(rename = "perSystem")]
    pub per_system: BTreeMap<String, WireIndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireSubstituters {
    pub urls: Vec<String>,
    #[serde(rename = "trustedPublicKeys")]
    pub trusted_public_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireNativeBuildEntry {
    pub mode: BuildMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireBuildPolicy {
    #[serde(rename = "nativeLocalBuilds")]
    pub native_local_builds: BTreeMap<String, WireNativeBuildEntry>,
}

/// Strict wire representation of the authenticated `descriptor.json` target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireDescriptor {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u64,
    pub channel: String,
    #[serde(rename = "policyVersion")]
    pub policy_version: u64,
    pub sequence: u64,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
    #[serde(rename = "supportedSystems")]
    pub supported_systems: Vec<String>,
    #[serde(rename = "buildPolicy")]
    pub build_policy: WireBuildPolicy,
    #[serde(rename = "nixRuntime")]
    pub nix_runtime: WireNixRuntime,
    pub nixpkgs: WireNixpkgs,
    pub index: WireIndex,
    pub substituters: WireSubstituters,
}

/// Authenticated Nix runtime artifact for one native system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixRuntimeArtifact {
    pub(crate) url: String,
    pub(crate) sha256: String,
    pub(crate) target: String,
    pub(crate) asset_manifest_target: String,
    pub(crate) asset_manifest_sha256: String,
}

impl NixRuntimeArtifact {
    /// Returns the authenticated HTTPS download URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the lowercase SHA-256 digest bound to TUF target metadata.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns the authenticated TUF target carrying the runtime archive.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the authenticated TUF target carrying the static asset manifest.
    #[must_use]
    pub fn asset_manifest_target(&self) -> &str {
        &self.asset_manifest_target
    }

    /// Returns the lowercase SHA-256 of the canonical static asset manifest.
    #[must_use]
    pub fn asset_manifest_sha256(&self) -> &str {
        &self.asset_manifest_sha256
    }
}

/// Authenticated index target for one native system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexArtifact {
    pub(crate) target: String,
    pub(crate) sha256: String,
}

impl IndexArtifact {
    /// Returns the authenticated TUF target name.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the lowercase SHA-256 digest bound to TUF target metadata.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// Authenticated direct Nixpkgs flake identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixpkgsPin {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) rev: NixpkgsRevision,
    pub(crate) nar_hash: NarHash,
}

/// One authenticated Nix binary-cache public key.
///
/// Construction is private to channel verification; consumers cannot inject
/// an arbitrary trust root after the descriptor has been authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePublicKey {
    pub(crate) value: String,
    pub(crate) name_end: usize,
}

impl CachePublicKey {
    pub(crate) fn from_validated(value: &str) -> Option<Self> {
        value.find(':').map(|name_end| Self {
            value: value.to_owned(),
            name_end,
        })
    }

    /// Returns the complete Nix public-key string (`name:base64`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Returns the key name used to identify observed path signatures.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.value[..self.name_end]
    }
}

/// Authenticated, fixed binary-cache policy retained from the descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePolicy {
    pub(crate) url: String,
    pub(crate) trusted_public_keys: Vec<CachePublicKey>,
}

impl CachePolicy {
    /// Returns the sole channel-approved substituter URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the authenticated trusted public keys.
    #[must_use]
    pub fn trusted_public_keys(&self) -> &[CachePublicKey] {
        &self.trusted_public_keys
    }

    /// Returns whether an observed signature names an authenticated cache key.
    #[must_use]
    pub fn admits_signature_name(&self, name: &str) -> bool {
        self.trusted_public_keys
            .iter()
            .any(|key| key.name() == name)
    }
}

impl NixpkgsPin {
    /// Returns the authenticated repository owner.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Returns the authenticated repository name.
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Returns the canonical 40-character Nixpkgs revision.
    #[must_use]
    pub fn revision(&self) -> &str {
        self.rev.as_str()
    }

    /// Returns the canonical SRI NAR hash.
    #[must_use]
    pub fn nar_hash(&self) -> &str {
        self.nar_hash.as_str()
    }
}

/// Product-facing channel descriptor after semantic validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDescriptor {
    pub(crate) channel: ChannelName,
    pub(crate) expires_at: jiff::Timestamp,
    pub(crate) nix_version: String,
    pub(crate) runtime: NixRuntimeArtifact,
    pub(crate) nixpkgs: NixpkgsPin,
    pub(crate) index: IndexArtifact,
    pub(crate) cache: CachePolicy,
    pub(crate) build_mode: BuildMode,
}

impl ChannelDescriptor {
    /// Returns the display channel name.
    #[must_use]
    pub fn channel(&self) -> &str {
        self.channel.as_str()
    }

    /// Returns the product-policy expiry timestamp.
    #[must_use]
    pub const fn expires_at(&self) -> jiff::Timestamp {
        self.expires_at
    }

    /// Returns the pinned managed-Nix runtime version.
    #[must_use]
    pub fn nix_version(&self) -> &str {
        &self.nix_version
    }

    /// Returns the host's authenticated managed-Nix artifact.
    #[must_use]
    pub const fn runtime(&self) -> &NixRuntimeArtifact {
        &self.runtime
    }

    /// Returns the authenticated Nixpkgs flake pin.
    #[must_use]
    pub const fn nixpkgs(&self) -> &NixpkgsPin {
        &self.nixpkgs
    }

    /// Returns the host's authenticated package-index artifact.
    #[must_use]
    pub const fn index(&self) -> &IndexArtifact {
        &self.index
    }

    /// Returns the authenticated binary-cache trust policy.
    #[must_use]
    pub const fn cache(&self) -> &CachePolicy {
        &self.cache
    }

    /// Returns the host's native local-build policy.
    #[must_use]
    pub const fn build_mode(&self) -> BuildMode {
        self.build_mode
    }
}
