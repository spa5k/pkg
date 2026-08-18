//! Online-role TUF signing from an already signed offline root.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use sha2::{Digest, Sha256};
use tough::TargetName;
use tough::editor::RepositoryEditor;
use tough::editor::signed::PathExists;
use tough::key_source::KeySource;
use tough::schema::decoded::{Decoded, Hex};
use tough::schema::{Hashes, Root, Signed, Target};
use tough::{ExpirationEnforcement, FilesystemTransport, IntoVec, RepositoryLoader};
use url::Url;

use crate::{
    AuditEvent, DurableRelease, PublicationError, PublicationObject, ValidatedRelease,
    ValidationError, write_audit_log,
};

/// Versions and expirations for a single metadata publication.
#[derive(Debug, Clone, Copy)]
pub struct MetadataPolicy {
    /// Monotonic targets version.
    pub targets_version: NonZeroU64,
    /// Monotonic snapshot version.
    pub snapshot_version: NonZeroU64,
    /// Monotonic timestamp version.
    pub timestamp_version: NonZeroU64,
    /// Targets expiration.
    pub targets_expires: Timestamp,
    /// Snapshot expiration.
    pub snapshot_expires: Timestamp,
    /// Short timestamp expiration.
    pub timestamp_expires: Timestamp,
}

/// Sealed output of a successful signing transaction.
pub struct SignedRelease {
    pub(crate) release: ValidatedRelease,
    pub(crate) objects: Vec<PublicationObject>,
}

impl SignedRelease {
    /// Returns the immutable objects that will be published.
    #[must_use]
    pub fn objects(&self) -> &[PublicationObject] {
        &self.objects
    }

    /// Atomically persists exact bytes and lease identity before publication.
    pub fn persist(self, directory: &Path) -> Result<DurableRelease, PublicationError> {
        DurableRelease::persist(self, directory)
    }
}

/// TUF signing or immutable-output failure.
#[derive(Debug)]
pub enum SignError {
    /// A filesystem boundary was unsafe or unavailable.
    Filesystem,
    /// TUF refused the signed root, target, keys, or metadata policy.
    Tuf(Box<tough::error::Error>),
    /// A source could not be represented as a TUF target.
    Target(Box<tough::schema::Error>),
    /// An artifact changed after manifest validation.
    Validation(ValidationError),
    /// An online KMS/HSM key source could not expose its public signer.
    KeySource,
    /// The signed repository could not be sealed into immutable publication files.
    Publication(PublicationError),
}

impl fmt::Display for SignError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Filesystem => formatter.write_str("release output boundary is unavailable"),
            Self::Tuf(_) => formatter.write_str("TUF signing failed"),
            Self::Target(_) => formatter.write_str("TUF target construction failed"),
            Self::Validation(_) => formatter.write_str("release artifacts changed before signing"),
            Self::KeySource => formatter.write_str("online signing key source failed"),
            Self::Publication(_) => formatter.write_str("release publication sealing failed"),
        }
    }
}

impl std::error::Error for SignError {}

impl From<tough::error::Error> for SignError {
    fn from(error: tough::error::Error) -> Self {
        Self::Tuf(Box::new(error))
    }
}

impl From<tough::schema::Error> for SignError {
    fn from(error: tough::schema::Error) -> Self {
        Self::Target(Box::new(error))
    }
}

impl From<ValidationError> for SignError {
    fn from(error: ValidationError) -> Self {
        Self::Validation(error)
    }
}

/// Signs targets/snapshot/timestamp and copies verified consistent-snapshot targets.
///
/// `root_path` is signed offline input. Root private keys are not an argument;
/// `online_keys` should be KMS/HSM-backed `KeySource` implementations for only
/// the targets, snapshot, and timestamp roles. `output` must not already exist.
pub async fn sign_channel(
    release: ValidatedRelease,
    root_path: &Path,
    online_keys: &[Box<dyn KeySource>],
    policy: MetadataPolicy,
    output: &Path,
) -> Result<SignedRelease, SignError> {
    release.revalidate_all()?;
    validate_metadata_policy(&release, policy)?;
    let signing_actor = release.signing_actor()?.to_owned();
    let root_metadata = fs::symlink_metadata(root_path).map_err(|_| SignError::Filesystem)?;
    if !root_metadata.is_file() || root_metadata.file_type().is_symlink() || output.exists() {
        return Err(SignError::Filesystem);
    }
    let output = absolute_output_path(output)?;
    let root_path = fs::canonicalize(root_path).map_err(|_| SignError::Filesystem)?;
    let root_bytes = fs::read(&root_path).map_err(|_| SignError::Filesystem)?;
    if hex::encode(Sha256::digest(&root_bytes)) != release.trusted_root_sha256() {
        return Err(SignError::Filesystem);
    }
    let root: Signed<Root> =
        serde_json::from_slice(&root_bytes).map_err(|_| SignError::Filesystem)?;
    root.signed.verify_role(&root)?;
    if !root.signed.consistent_snapshot || root.signed.expires <= policy.targets_expires {
        return Err(SignError::Filesystem);
    }
    let root_version = root.signed.version.get();
    let metadata_dir = output.join("metadata");
    let targets_dir = output.join("targets");
    fs::create_dir(&output).map_err(|_| SignError::Filesystem)?;
    let mut output_guard = OutputGuard {
        path: &output,
        committed: false,
    };
    fs::create_dir(&metadata_dir).map_err(|_| SignError::Filesystem)?;
    fs::create_dir(&targets_dir).map_err(|_| SignError::Filesystem)?;

    let mut key_ids = Vec::new();
    for source in online_keys {
        let signer = source.as_sign().await.map_err(|_| SignError::KeySource)?;
        key_ids.push(hex::encode(
            signer
                .tuf_key()
                .key_id()
                .map_err(|_| SignError::KeySource)?
                .as_ref(),
        ));
    }
    key_ids.sort();
    key_ids.dedup();

    let mut editor = RepositoryEditor::new(&root_path).await?;
    editor
        .targets_version(policy.targets_version)?
        .targets_expires(policy.targets_expires)?
        .snapshot_version(policy.snapshot_version)
        .snapshot_expires(policy.snapshot_expires)
        .timestamp_version(policy.timestamp_version)
        .timestamp_expires(policy.timestamp_expires);
    for (name, _, digest, length) in release.tuf_targets() {
        let target_name = TargetName::new(name).map_err(|_| SignError::Filesystem)?;
        let target = target_from_manifest(digest, length)?;
        editor.add_target(target_name, target)?;
    }
    let signed = editor.sign(online_keys).await?;
    signed.write(&metadata_dir).await?;
    for (name, source, _, _) in release.tuf_targets() {
        let target_name = TargetName::new(name).map_err(|_| SignError::Filesystem)?;
        signed
            .copy_target(&source, &targets_dir, PathExists::Fail, Some(&target_name))
            .await?;
    }
    fs::write(
        output.join("release-manifest.json"),
        release.canonical_manifest(),
    )
    .map_err(|_| SignError::Filesystem)?;
    let signed_at = Timestamp::now().to_string();
    write_audit_log(
        &output.join("signing-audit.ndjson"),
        &AuditEvent {
            schema_version: 1,
            release_id: release.release_id(),
            release_digest: release.release_digest(),
            actor: &signing_actor,
            key_ids: &key_ids,
            signed_at: &signed_at,
        },
    )
    .map_err(|_| SignError::Filesystem)?;
    verify_repository(&root_bytes, &release, &metadata_dir, &targets_dir).await?;
    let objects = crate::publish::seal_objects(&release, &output, root_version)
        .map_err(SignError::Publication)?;
    output_guard.committed = true;
    Ok(SignedRelease { release, objects })
}

fn absolute_output_path(output: &Path) -> Result<PathBuf, SignError> {
    let name = output.file_name().ok_or(SignError::Filesystem)?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(fs::canonicalize(parent)
        .map_err(|_| SignError::Filesystem)?
        .join(name))
}

async fn verify_repository(
    root: &[u8],
    release: &ValidatedRelease,
    metadata: &Path,
    targets: &Path,
) -> Result<(), SignError> {
    let datastore = tempfile::tempdir().map_err(|_| SignError::Filesystem)?;
    let repository = RepositoryLoader::new(
        &root,
        Url::from_directory_path(metadata).map_err(|()| SignError::Filesystem)?,
        Url::from_directory_path(targets).map_err(|()| SignError::Filesystem)?,
    )
    .transport(FilesystemTransport)
    .expiration_enforcement(ExpirationEnforcement::Safe)
    .datastore(datastore.path())
    .load()
    .await?;
    for (name, _, _, _) in release.tuf_targets() {
        let target = TargetName::new(name).map_err(|_| SignError::Filesystem)?;
        let stream = repository
            .read_target(&target)
            .await?
            .ok_or(SignError::Filesystem)?;
        IntoVec::<tough::error::Error>::into_vec(stream).await?;
    }
    Ok(())
}

fn target_from_manifest(digest: &str, length: u64) -> Result<Target, SignError> {
    let digest = hex::decode(digest).map_err(|_| SignError::Filesystem)?;
    Ok(Target {
        length,
        hashes: Hashes {
            sha256: Decoded::<Hex>::from(digest),
            _extra: HashMap::new(),
        },
        custom: HashMap::new(),
        _extra: HashMap::new(),
    })
}

fn validate_metadata_policy(
    release: &ValidatedRelease,
    policy: MetadataPolicy,
) -> Result<(), SignError> {
    let sequence = release.channel_sequence();
    let now = Timestamp::now();
    let max_timestamp = now + jiff::SignedDuration::from_hours(48);
    let max_targets = now + jiff::SignedDuration::from_hours(24 * 90);
    if policy.targets_version.get() != sequence
        || policy.snapshot_version.get() != sequence
        || policy.timestamp_version.get() != release.timestamp_version()
        || policy.timestamp_expires <= now
        || policy.timestamp_expires > max_timestamp
        || policy.snapshot_expires < policy.timestamp_expires
        || policy.targets_expires < policy.snapshot_expires
        || policy.targets_expires > max_targets
    {
        return Err(SignError::Filesystem);
    }
    Ok(())
}

struct OutputGuard<'a> {
    path: &'a Path,
    committed: bool,
}

impl Drop for OutputGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::error::Error;
    use std::fs;
    use std::io::Read;
    use std::num::NonZeroU64;
    use std::path::Path;

    use async_trait::async_trait;
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::signature::Ed25519KeyPair;
    use olpc_cjson::CanonicalFormatter;
    use serde::Serialize;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tough::key_source::KeySource;
    use tough::schema::decoded::{Decoded, Hex};
    use tough::schema::key::Key;
    use tough::schema::{RoleKeys, RoleType, Root, Signature, Signed};
    use tough::sign::{Sign, parse_keypair};
    use tough::{
        ExpirationEnforcement, FilesystemTransport, IntoVec, RepositoryLoader, TargetName,
    };
    use url::Url;

    use super::{MetadataPolicy, absolute_output_path, sign_channel};
    use crate::{
        Approval, DurableRelease, ReleaseAuthority, ReleaseAuthorization, ReleaseManifest,
        TimestampAuthority, TimestampAuthorization, ValidationError, refresh_timestamp,
    };

    struct TestAuthority;
    struct TestAuthorization;

    #[test]
    fn relative_output_path_is_made_absolute() {
        let output = absolute_output_path(Path::new("preview-output")).expect("absolute output");
        assert!(output.is_absolute());
        assert_eq!(
            output.file_name().and_then(|name| name.to_str()),
            Some("preview-output")
        );
    }
    struct TestTimestampAuthority {
        trusted_root_sha256: String,
    }
    struct TestTimestampAuthorization;

    impl ReleaseAuthorization for TestAuthorization {
        fn lease_id(&self) -> &str {
            "release-lease-1"
        }

        fn signing_actor(&self) -> &str {
            "release-service"
        }

        fn bind_transaction(&mut self, transaction_digest: &str) -> Result<(), ValidationError> {
            if transaction_digest.len() != 64 {
                return Err(ValidationError::InvalidPolicy);
            }
            Ok(())
        }

        fn commit(&mut self) -> Result<(), ValidationError> {
            Ok(())
        }
    }

    impl ReleaseAuthority for TestAuthority {
        fn authorize(
            &self,
            release_digest: &str,
            sequence: u64,
            timestamp_version: u64,
            approvals: &[Approval],
        ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError> {
            let evidence: Vec<_> = approvals.iter().map(Approval::evidence).collect();
            if sequence != 1
                || release_digest.len() != 64
                || timestamp_version != 1
                || evidence != ["oidc:release-owner", "oidc:security-owner"]
            {
                return Err(ValidationError::InvalidPolicy);
            }
            Ok(Box::new(TestAuthorization))
        }

        fn resume(
            &self,
            release_digest: &str,
            transaction_digest: &str,
            lease_id: &str,
        ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError> {
            if release_digest.len() != 64
                || transaction_digest.len() != 64
                || lease_id != "release-lease-1"
            {
                return Err(ValidationError::InvalidPolicy);
            }
            Ok(Box::new(TestAuthorization))
        }
    }

    impl TimestampAuthorization for TestTimestampAuthorization {
        fn lease_id(&self) -> &str {
            "timestamp-lease-2"
        }

        fn signing_actor(&self) -> &str {
            "timestamp-service"
        }

        fn bind_transaction(&mut self, transaction_digest: &str) -> Result<(), ValidationError> {
            if transaction_digest.len() != 64 {
                return Err(ValidationError::InvalidPolicy);
            }
            Ok(())
        }

        fn commit(&mut self) -> Result<(), ValidationError> {
            Ok(())
        }
    }

    impl TimestampAuthority for TestTimestampAuthority {
        fn authorize(
            &self,
            release_id: &str,
            trusted_root_sha256: &str,
            snapshot_digest: &str,
            snapshot_version: u64,
            timestamp_version: u64,
        ) -> Result<Box<dyn TimestampAuthorization>, ValidationError> {
            if release_id != "v0.1.0"
                || trusted_root_sha256 != self.trusted_root_sha256
                || snapshot_digest.len() != 64
                || snapshot_version != 1
                || timestamp_version != 2
            {
                return Err(ValidationError::InvalidPolicy);
            }
            Ok(Box::new(TestTimestampAuthorization))
        }

        fn resume(
            &self,
            release_id: &str,
            timestamp_version: u64,
            transaction_digest: &str,
            lease_id: &str,
        ) -> Result<Box<dyn TimestampAuthorization>, ValidationError> {
            if release_id != "v0.1.0"
                || timestamp_version != 2
                || transaction_digest.len() != 64
                || lease_id != "timestamp-lease-2"
            {
                return Err(ValidationError::InvalidPolicy);
            }
            Ok(Box::new(TestTimestampAuthorization))
        }
    }

    #[derive(Clone)]
    struct TestKey {
        pkcs8: Vec<u8>,
        key: Key,
        id: Decoded<Hex>,
    }

    impl std::fmt::Debug for TestKey {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("TestKey(<redacted>)")
        }
    }

    impl TestKey {
        fn generate() -> Self {
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .expect("ephemeral test key")
                .as_ref()
                .to_vec();
            let signer = parse_keypair(&pkcs8).expect("tough parses test key");
            let key = signer.tuf_key();
            let id = key.key_id().expect("test key id");
            Self { pkcs8, key, id }
        }

        fn signer(&self) -> Box<dyn Sign> {
            Box::new(parse_keypair(&self.pkcs8).expect("parse ephemeral test key"))
        }
    }

    #[async_trait]
    impl KeySource for TestKey {
        async fn as_sign(&self) -> Result<Box<dyn Sign>, Box<dyn Error + Send + Sync + 'static>> {
            Ok(self.signer())
        }

        async fn write(
            &self,
            _value: &str,
            _key_id_hex: &str,
        ) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
            Err("test keys are memory-only".into())
        }
    }

    async fn signed_root(
        root_keys: &[TestKey],
        online: &[TestKey],
        expires: jiff::Timestamp,
    ) -> Vec<u8> {
        let mut keys = HashMap::new();
        for key in root_keys.iter().chain(online) {
            keys.insert(key.id.clone(), key.key.clone());
        }
        let role = |items: &[TestKey], threshold: u64| RoleKeys {
            keyids: items.iter().map(|key| key.id.clone()).collect(),
            threshold: NonZeroU64::new(threshold).expect("nonzero threshold"),
            _extra: HashMap::new(),
        };
        let mut roles = HashMap::new();
        roles.insert(RoleType::Root, role(root_keys, 2));
        roles.insert(RoleType::Targets, role(&online[0..1], 1));
        roles.insert(RoleType::Snapshot, role(&online[1..2], 1));
        roles.insert(RoleType::Timestamp, role(&online[2..3], 1));
        let root = Root {
            spec_version: "1.0.0".into(),
            consistent_snapshot: true,
            version: NonZeroU64::new(1).expect("nonzero"),
            expires,
            keys,
            roles,
            _extra: HashMap::new(),
        };
        let mut canonical = Vec::new();
        let mut serializer =
            serde_json::Serializer::with_formatter(&mut canonical, CanonicalFormatter::new());
        root.serialize(&mut serializer).expect("canonical root");
        let mut signatures = Vec::new();
        for key in root_keys.iter().take(2) {
            let signature = key
                .signer()
                .sign(&canonical, &SystemRandom::new())
                .await
                .expect("sign test root");
            signatures.push(Signature {
                keyid: key.id.clone(),
                sig: signature.into(),
            });
        }
        let mut bytes = serde_json::to_vec_pretty(&Signed {
            signed: root,
            signatures,
        })
        .expect("serialize root");
        bytes.push(b'\n');
        bytes
    }

    fn write_file(root: &Path, relative: &str, bytes: &[u8]) -> (String, u64) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(path, bytes).expect("fixture file");
        (hex::encode(Sha256::digest(bytes)), bytes.len() as u64)
    }

    fn release_fixture_json(root: &Path) -> serde_json::Value {
        release_fixture_json_with_root(root, &"a".repeat(64))
    }

    fn release_fixture_json_with_root(root: &Path, trusted_root_sha256: &str) -> serde_json::Value {
        let systems = ["aarch64-darwin", "x86_64-linux"];
        let mut artifacts = Vec::new();
        let (digest, length) = write_file(root, "descriptor.json", b"fixture descriptor\n");
        artifacts.push(serde_json::json!({"kind":"descriptor","system":null,"target":"descriptor.json","source":"descriptor.json","sha256":digest,"length":length}));
        for system in systems {
            for (kind, target) in [
                ("managed-nix", format!("nix/2.34.8/{system}.tar.xz")),
                (
                    "managed-nix-assets",
                    format!("nix/2.34.8/{system}.assets.json"),
                ),
                ("index", format!("index/1/{system}.json.br")),
            ] {
                let source = target.clone();
                let bytes = format!("{kind} {system}\n");
                let (digest, length) = write_file(root, &source, bytes.as_bytes());
                artifacts.push(serde_json::json!({"kind":kind,"system":system,"target":target,"source":source,"sha256":digest,"length":length}));
            }
            for name in ["pkg-root-helper", "pkg-nix-broker", "pkg"] {
                let target = format!("installer/{system}/{name}");
                let source = target.clone();
                let bytes = format!("installer payload {name} {system}\n");
                let (digest, length) = write_file(root, &source, bytes.as_bytes());
                artifacts.push(serde_json::json!({"kind":"installer-payload","system":system,"target":target,"source":source,"sha256":digest,"length":length}));
            }
        }
        let mut cli = Vec::new();
        for system in ["aarch64-darwin", "x86_64-linux"] {
            let source = format!("cli/pkg-{system}");
            let bundle = format!("cli/pkg-{system}.sigstore.json");
            let (digest, length) = write_file(root, &source, system.as_bytes());
            let (bundle_digest, bundle_length) =
                write_file(root, &bundle, b"fixture sigstore bundle\n");
            cli.push(serde_json::json!({"kind":"pkg","system":system,"source":source,"sha256":digest,"length":length,"sigstoreBundle":bundle,"sigstoreBundleSha256":bundle_digest,"sigstoreBundleLength":bundle_length}));
        }
        {
            let system = "x86_64-linux";
            let source = format!("cli/pkg-installer-{system}");
            let bundle = format!("cli/pkg-installer-{system}.sigstore.json");
            let (digest, length) = write_file(root, &source, system.as_bytes());
            let (bundle_digest, bundle_length) =
                write_file(root, &bundle, b"fixture installer sigstore bundle\n");
            cli.push(serde_json::json!({"kind":"pkg-install","system":system,"source":source,"sha256":digest,"length":length,"sigstoreBundle":bundle,"sigstoreBundleSha256":bundle_digest,"sigstoreBundleLength":bundle_length}));
        }
        serde_json::json!({
            "schemaVersion":1,"releaseId":"v0.1.0","channelSequence":1,"timestampVersion":1,"policyVersion":1,
            "trustedRootSha256":trusted_root_sha256,
            "artifacts":artifacts,"cliArtifacts":cli,
            "approvals":[
                {"actor":"release-owner","role":"release","evidence":"oidc:release-owner"},
                {"actor":"security-owner","role":"security","evidence":"oidc:security-owner"}
            ]
        })
    }

    fn release_fixture(root: &Path) -> crate::ValidatedRelease {
        let manifest = release_fixture_json(root);
        ReleaseManifest::from_json(
            &serde_json::to_vec(&manifest).expect("manifest"),
            root,
            &TestAuthority,
        )
        .expect("valid fixture release")
    }

    fn release_fixture_with_root(
        root: &Path,
        trusted_root_sha256: &str,
    ) -> crate::ValidatedRelease {
        let manifest = release_fixture_json_with_root(root, trusted_root_sha256);
        ReleaseManifest::from_json(
            &serde_json::to_vec(&manifest).expect("manifest"),
            root,
            &TestAuthority,
        )
        .expect("valid trusted-root fixture release")
    }

    #[test]
    fn manifest_refuses_forged_approval_extended_schema_and_target_confusion() {
        let temporary = TempDir::new().expect("temporary release");
        let root = temporary.path();
        let original = release_fixture_json(root);

        let mut forged = original.clone();
        forged["approvals"][1]["actor"] = serde_json::json!("release-owner");
        assert!(ReleaseManifest::from_json(
            &serde_json::to_vec(&forged).unwrap(),
            root,
            &TestAuthority,
        )
        .is_err());

        let mut extended = original.clone();
        extended["unreviewed"] = serde_json::json!(true);
        assert!(
            ReleaseManifest::from_json(
                &serde_json::to_vec(&extended).unwrap(),
                root,
                &TestAuthority,
            )
            .is_err()
        );

        let mut confused = original;
        confused["artifacts"][0]["target"] = serde_json::json!("cli/pkg-aarch64-darwin");
        assert!(
            ReleaseManifest::from_json(
                &serde_json::to_vec(&confused).unwrap(),
                root,
                &TestAuthority,
            )
            .is_err()
        );

        let mut missing_payload = release_fixture_json(root);
        missing_payload["artifacts"]
            .as_array_mut()
            .unwrap()
            .retain(|artifact| artifact["target"] != "installer/x86_64-linux/pkg-root-helper");
        assert!(
            ReleaseManifest::from_json(
                &serde_json::to_vec(&missing_payload).unwrap(),
                root,
                &TestAuthority,
            )
            .is_err()
        );

        let mut missing_bootstrap = release_fixture_json(root);
        missing_bootstrap["cliArtifacts"]
            .as_array_mut()
            .unwrap()
            .retain(|artifact| {
                artifact["kind"] != "pkg-install" || artifact["system"] != "x86_64-linux"
            });
        assert!(
            ReleaseManifest::from_json(
                &serde_json::to_vec(&missing_bootstrap).unwrap(),
                root,
                &TestAuthority,
            )
            .is_err()
        );

        let mut unauthenticated = release_fixture_json(root);
        unauthenticated["approvals"][1]["evidence"] = serde_json::json!("oidc:someone-else");
        assert!(
            ReleaseManifest::from_json(
                &serde_json::to_vec(&unauthenticated).unwrap(),
                root,
                &TestAuthority,
            )
            .is_err()
        );

        let mut stale = release_fixture_json(root);
        stale["channelSequence"] = serde_json::json!(2);
        for artifact in stale["artifacts"].as_array_mut().unwrap() {
            if artifact["kind"] == "index" {
                let target = artifact["target"]
                    .as_str()
                    .unwrap()
                    .replace("index/1/", "index/2/");
                artifact["target"] = serde_json::json!(target);
            }
        }
        assert!(
            ReleaseManifest::from_json(&serde_json::to_vec(&stale).unwrap(), root, &TestAuthority,)
                .is_err()
        );
    }

    #[test]
    fn manifest_refuses_a_symlinked_source_ancestor() {
        let temporary = TempDir::new().expect("temporary release");
        let root = temporary.path();
        let manifest = release_fixture_json(root);
        fs::rename(root.join("cli"), root.join("cli-real")).expect("move fixture directory");
        std::os::unix::fs::symlink("cli-real", root.join("cli")).expect("fixture symlink");
        assert!(
            ReleaseManifest::from_json(
                &serde_json::to_vec(&manifest).unwrap(),
                root,
                &TestAuthority,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn dry_run_signs_threshold_root_repo_and_client_verifies() {
        let temporary = TempDir::new().expect("temporary release");
        let artifact_root = temporary.path().join("artifacts");
        fs::create_dir(&artifact_root).expect("artifact root");
        let root_keys = [
            TestKey::generate(),
            TestKey::generate(),
            TestKey::generate(),
        ];
        let online = [
            TestKey::generate(),
            TestKey::generate(),
            TestKey::generate(),
        ];
        let now = jiff::Timestamp::now();
        let root_path = temporary.path().join("root.json");
        let root_bytes = signed_root(
            &root_keys,
            &online,
            now + jiff::SignedDuration::from_hours(24 * 365),
        )
        .await;
        fs::write(&root_path, &root_bytes).expect("offline root fixture");
        let release =
            release_fixture_with_root(&artifact_root, &hex::encode(Sha256::digest(&root_bytes)));
        let keys: Vec<Box<dyn KeySource>> = online
            .iter()
            .cloned()
            .map(|key| Box::new(key) as Box<dyn KeySource>)
            .collect();
        let output = temporary.path().join("repository");
        let signed = sign_channel(
            release,
            &root_path,
            &keys,
            MetadataPolicy {
                targets_version: NonZeroU64::new(1).expect("nonzero"),
                snapshot_version: NonZeroU64::new(1).expect("nonzero"),
                timestamp_version: NonZeroU64::new(1).expect("nonzero"),
                targets_expires: now + jiff::SignedDuration::from_hours(24 * 30),
                snapshot_expires: now + jiff::SignedDuration::from_hours(24 * 7),
                timestamp_expires: now + jiff::SignedDuration::from_hours(24),
            },
            &output,
        )
        .await
        .expect("dry-run sign");
        let publication = signed.objects();
        assert_eq!(publication.len(), crate::publish::RELEASE_OBJECT_COUNT);
        assert!(
            publication
                .iter()
                .any(|object| object.name().ends_with("signing-audit.ndjson"))
        );
        assert!(publication.iter().any(|object| {
            object
                .name()
                .ends_with("cli/pkg-installer-x86_64-linux.sigstore.json")
        }));
        assert!(!publication.iter().any(|object| {
            object.name().contains("/targets/") && object.name().contains("pkg-install")
        }));

        let timestamp_keys: Vec<Box<dyn KeySource>> =
            vec![Box::new(online[2].clone()) as Box<dyn KeySource>];
        let timestamp_authority = TestTimestampAuthority {
            trusted_root_sha256: hex::encode(Sha256::digest(&root_bytes)),
        };
        let refresh = refresh_timestamp(
            "v0.1.0",
            &root_path,
            &output.join("metadata/1.snapshot.json"),
            NonZeroU64::new(2).expect("nonzero"),
            now + jiff::SignedDuration::from_hours(24),
            &timestamp_authority,
            &timestamp_keys,
        )
        .await
        .expect("independent timestamp refresh");
        assert_eq!(refresh.timestamp_version(), 2);
        assert_eq!(refresh.objects().len(), 2);
        let mut refreshed_timestamp = Vec::new();
        refresh.objects()[0]
            .reader()
            .read_to_end(&mut refreshed_timestamp)
            .expect("sealed timestamp bytes");
        let parsed: Signed<tough::schema::Timestamp> =
            serde_json::from_slice(&refreshed_timestamp).expect("timestamp JSON");
        assert_eq!(parsed.signed.version.get(), 2);
        let trusted_root: Signed<Root> =
            serde_json::from_slice(&root_bytes).expect("trusted root JSON");
        trusted_root
            .signed
            .verify_role(&parsed)
            .expect("root verifies refreshed timestamp");
        let short_root_path = temporary.path().join("short-root.json");
        fs::write(
            &short_root_path,
            signed_root(
                &root_keys,
                &online,
                now + jiff::SignedDuration::from_hours(12),
            )
            .await,
        )
        .expect("short root fixture");
        assert!(
            refresh_timestamp(
                "v0.1.0",
                &short_root_path,
                &output.join("metadata/1.snapshot.json"),
                NonZeroU64::new(2).expect("nonzero"),
                now + jiff::SignedDuration::from_hours(24),
                &timestamp_authority,
                &timestamp_keys,
            )
            .await
            .is_err()
        );

        fs::write(
            artifact_root.join("cli/pkg-aarch64-darwin"),
            b"changed after signing",
        )
        .expect("mutate CLI fixture");
        let cli = signed
            .objects()
            .iter()
            .find(|object| object.name().ends_with("cli/pkg-aarch64-darwin"))
            .expect("sealed CLI object");
        let mut cli_bytes = Vec::new();
        cli.reader()
            .read_to_end(&mut cli_bytes)
            .expect("sealed CLI");
        assert_eq!(cli_bytes, b"aarch64-darwin");

        let datastore = temporary.path().join("datastore");
        fs::create_dir(&datastore).expect("datastore");
        let repository = RepositoryLoader::new(
            &root_bytes,
            Url::from_directory_path(output.join("metadata")).expect("metadata URL"),
            Url::from_directory_path(output.join("targets")).expect("targets URL"),
        )
        .transport(FilesystemTransport)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .datastore(&datastore)
        .load()
        .await
        .expect("client verifies dry-run repository");
        let stream = repository
            .read_target(&TargetName::new("descriptor.json").expect("target name"))
            .await
            .expect("target read")
            .expect("target exists");
        let bytes = IntoVec::<tough::error::Error>::into_vec(stream)
            .await
            .expect("fully verified target");
        assert_eq!(bytes, b"fixture descriptor\n");
        let stream = repository
            .read_target(
                &TargetName::new("installer/x86_64-linux/pkg-root-helper")
                    .expect("installer target name"),
            )
            .await
            .expect("installer target read")
            .expect("installer target exists");
        let bytes = IntoVec::<tough::error::Error>::into_vec(stream)
            .await
            .expect("fully verified installer target");
        assert_eq!(bytes, b"installer payload pkg-root-helper x86_64-linux\n");
        assert!(!output.join("targets/pkg-aarch64-darwin").exists());
        let transaction_path = temporary.path().join("release-transaction");
        let durable = signed
            .persist(&transaction_path)
            .expect("persist release before publication");
        drop(durable);
        let resumed = DurableRelease::resume(&transaction_path, &TestAuthority)
            .expect("resume release after process loss");
        assert_eq!(
            resumed.directory(),
            fs::canonicalize(transaction_path).expect("canonical transaction")
        );
    }

    #[tokio::test]
    async fn existing_output_and_missing_threshold_key_fail_closed() {
        let temporary = TempDir::new().expect("temporary release");
        let artifacts = temporary.path().join("artifacts");
        fs::create_dir(&artifacts).expect("artifacts");
        let root_keys = [
            TestKey::generate(),
            TestKey::generate(),
            TestKey::generate(),
        ];
        let online = [
            TestKey::generate(),
            TestKey::generate(),
            TestKey::generate(),
        ];
        let now = jiff::Timestamp::now();
        let root_path = temporary.path().join("root.json");
        let root_bytes = signed_root(
            &root_keys,
            &online,
            now + jiff::SignedDuration::from_hours(48),
        )
        .await;
        fs::write(&root_path, &root_bytes).expect("root");
        let root_digest = hex::encode(Sha256::digest(&root_bytes));
        let release = release_fixture_with_root(&artifacts, &root_digest);
        let output = temporary.path().join("exists");
        fs::create_dir(&output).expect("existing output");
        let policy = MetadataPolicy {
            targets_version: NonZeroU64::new(1).expect("nonzero"),
            snapshot_version: NonZeroU64::new(1).expect("nonzero"),
            timestamp_version: NonZeroU64::new(1).expect("nonzero"),
            targets_expires: now + jiff::SignedDuration::from_hours(24),
            snapshot_expires: now + jiff::SignedDuration::from_hours(24),
            timestamp_expires: now + jiff::SignedDuration::from_hours(12),
        };
        assert!(
            sign_channel(release, &root_path, &[], policy, &output,)
                .await
                .is_err()
        );
        let fresh = temporary.path().join("fresh");
        let release = release_fixture_with_root(&artifacts, &root_digest);
        assert!(
            sign_channel(release, &root_path, &[], policy, &fresh,)
                .await
                .is_err()
        );
        assert!(!fresh.exists());

        let keys: Vec<Box<dyn KeySource>> = online
            .iter()
            .cloned()
            .map(|key| Box::new(key) as Box<dyn KeySource>)
            .collect();
        let release = release_fixture_with_root(&artifacts, &root_digest);
        let overlong = temporary.path().join("overlong-metadata");
        assert!(
            sign_channel(
                release,
                &root_path,
                &keys,
                MetadataPolicy {
                    targets_version: NonZeroU64::new(1).expect("nonzero"),
                    snapshot_version: NonZeroU64::new(1).expect("nonzero"),
                    timestamp_version: NonZeroU64::new(1).expect("nonzero"),
                    targets_expires: now + jiff::SignedDuration::from_hours(72),
                    snapshot_expires: now + jiff::SignedDuration::from_hours(48),
                    timestamp_expires: now + jiff::SignedDuration::from_hours(12),
                },
                &overlong,
            )
            .await
            .is_err()
        );
        assert!(!overlong.exists());
    }

    #[tokio::test]
    async fn artifact_mutation_after_approval_is_refused_before_output() {
        let temporary = TempDir::new().expect("temporary release");
        let artifacts = temporary.path().join("artifacts");
        fs::create_dir(&artifacts).expect("artifacts");
        let release = release_fixture(&artifacts);
        fs::write(artifacts.join("descriptor.json"), b"mutated\n").expect("mutate fixture");
        let output = temporary.path().join("repository");
        let now = jiff::Timestamp::now();
        let policy = MetadataPolicy {
            targets_version: NonZeroU64::new(1).expect("nonzero"),
            snapshot_version: NonZeroU64::new(1).expect("nonzero"),
            timestamp_version: NonZeroU64::new(1).expect("nonzero"),
            targets_expires: now + jiff::SignedDuration::from_hours(24),
            snapshot_expires: now + jiff::SignedDuration::from_hours(24),
            timestamp_expires: now + jiff::SignedDuration::from_hours(12),
        };
        assert!(
            sign_channel(release, Path::new("unused-root"), &[], policy, &output,)
                .await
                .is_err()
        );
        assert!(!output.exists());
    }
}
