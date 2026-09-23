use super::*;
use pkg_core::{LockedFlake, PublicFlakeRef};
use std::io::Write;

pub(super) fn source_args() -> Vec<OsString> {
    let mut args = base_args();
    for (name, value) in [
        ("accept-flake-config", "false"),
        ("pure-eval", "true"),
        ("restrict-eval", "true"),
        ("allow-unsafe-native-code-during-evaluation", "false"),
        ("use-registries", "false"),
        ("allowed-uris", "https:// github: gitlab: git+https://"),
    ] {
        args.extend(os_args(["--option", name, value]));
    }
    args
}

impl RealNixAdapter {
    pub(super) fn lock_public_flake(
        &self,
        reference: &PublicFlakeRef,
    ) -> Result<LockedFlake, NixAdapterError> {
        let mut args = source_args();
        if self.eager_source_metadata {
            args.extend(os_args(["--option", "lazy-trees", "false"]));
        }
        args.extend(os_args([
            "flake",
            "metadata",
            "--json",
            "--no-update-lock-file",
            "--no-write-lock-file",
        ]));
        args.push(reference.source().into());
        let bytes = self.require_success(MethodKind::EvaluateDerivation, args, EVALUATE_TIMEOUT)?;
        if bytes.len() > 1024 * 1024 {
            return Err(NixAdapterError::OperationFailed);
        }
        let metadata: serde_json::Value = parse_json(&bytes)?;
        let locked = metadata.get("locked").ok_or_else(malformed)?;
        let field = |key: &str| {
            locked
                .get(key)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(malformed)
        };
        if field("type")? != "github"
            || format!("{}/{}", field("owner")?, field("repo")?) != reference.repository()
            || locked.get("host").is_some()
            || locked.get("dir").is_some()
        {
            return Err(NixAdapterError::OperationFailed);
        }
        let revision = pkg_core::NixpkgsRevision::new(field("rev")?).map_err(|_| malformed())?;
        let hash = if let Some(hash) = locked.get("narHash").and_then(serde_json::Value::as_str) {
            NarHash::new(hash).map_err(|_| malformed())?
        } else {
            self.flake_source_hash(&metadata)?
        };
        LockedFlake::new(
            reference.clone(),
            revision,
            hash,
            &metadata.get("locks").ok_or_else(malformed)?.to_string(),
        )
        .map_err(|_| malformed())
    }

    fn flake_source_hash(&self, metadata: &serde_json::Value) -> Result<NarHash, NixAdapterError> {
        // Determinate can omit narHash even with lazy trees disabled.
        let path = StorePath::new(
            metadata
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(malformed)?,
        )
        .map_err(|_| malformed())?;
        if path.as_str().ends_with(".drv") {
            return Err(malformed());
        }
        let mut args = base_args();
        args.extend(os_args(["hash", "path", "--sri", path.as_str()]));
        let bytes = self.require_success(MethodKind::EvaluateDerivation, args, EVALUATE_TIMEOUT)?;
        NarHash::new(std::str::from_utf8(&bytes).map_err(|_| malformed())?.trim())
            .map_err(|_| malformed())
    }

    pub(super) fn evaluate_public_flake(
        &self,
        request: &EvaluateDerivationRequest,
        lock: &LockedFlake,
    ) -> Result<DerivationPlanReport, NixAdapterError> {
        // Determinate's lock reader cannot traverse the broker's private TMPDIR.
        // Keep the file private (0600), but use the system temporary directory.
        let mut file =
            tempfile::NamedTempFile::new_in("/tmp").map_err(|_| NixAdapterError::Unavailable)?;
        file.write_all(lock.locks_json().as_bytes())
            .map_err(|_| NixAdapterError::Unavailable)?;
        file.flush().map_err(|_| NixAdapterError::Unavailable)?;
        let installable = format!(
            "github:{}/{}?narHash={}#{}",
            lock.reference().repository(),
            lock.revision(),
            percent_encode(lock.nar_hash().as_str()),
            request.attribute()
        );
        let mut args = source_args();
        args.extend(os_args([
            "derivation",
            "show",
            "--no-update-lock-file",
            "--no-write-lock-file",
            "--reference-lock-file",
        ]));
        args.push(file.path().as_os_str().to_owned());
        args.push(installable.into());
        let root_bytes = self.require_success(
            MethodKind::EvaluateDerivation,
            args.clone(),
            EVALUATE_TIMEOUT,
        )?;
        let root = single_derivation_name(&root_bytes)?;
        args.push("--recursive".into());
        let bytes = self.require_success(MethodKind::EvaluateDerivation, args, EVALUATE_TIMEOUT)?;
        normalize_derivation(&bytes, request, &root)
    }
}
