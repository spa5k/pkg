//! Cache substitution probes and path-info normalization.

use super::process::*;
use super::*;
pub(super) const PATH_INFO_FORMAT: u32 = 2;
pub(super) const CACHE_URL: &str = "https://cache.nixos.org";
pub(super) const CACHE_SIGNING_KEY_NAME: &str = "cache.nixos.org-1";
pub(super) const PATH_INFO_BATCH_SIZE: usize = 32;
pub(super) const MANAGED_STORE_PING_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const MANAGED_STORE_READY_WINDOW: Duration = Duration::from_secs(10);
pub(super) const MANAGED_STORE_RETRY_INTERVAL: Duration = Duration::from_millis(50);
pub(super) fn wait_for_managed_store_with(
    mut ping: impl FnMut(Duration) -> Result<(), NixAdapterError>,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
    window: Duration,
    interval: Duration,
    ping_timeout: Duration,
) -> Result<(), NixAdapterError> {
    let deadline = now().checked_add(window).ok_or(NixAdapterError::Timeout)?;
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Err(NixAdapterError::Timeout);
        }
        let result = ping(ping_timeout.min(remaining));
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Err(NixAdapterError::Timeout);
        }
        match result {
            Ok(()) => return Ok(()),
            Err(
                NixAdapterError::OperationFailed
                | NixAdapterError::Unavailable
                | NixAdapterError::Timeout,
            ) => {}
            Err(error) => return Err(error),
        }
        sleep(interval.min(remaining));
    }
}

impl BuildCacheProbe for RealNixAdapter {
    fn inspect(&self, paths: &[StorePath]) -> Result<Vec<CachePathObservation>, BuildCacheError> {
        if paths.is_empty() {
            return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
        }
        let mut local_ping = base_args();
        local_ping.extend(os_args(["store", "ping"]));
        self.require_success(MethodKind::PathInfo, local_ping, SHORT_TIMEOUT)
            .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;

        let mut observations = (0..paths.len()).map(|_| None).collect::<Vec<_>>();
        let mut remote_ready = false;
        for (chunk_index, chunk) in paths.chunks(PATH_INFO_BATCH_SIZE).enumerate() {
            let chunk_start = chunk_index * PATH_INFO_BATCH_SIZE;
            let path_refs = chunk.iter().collect::<Vec<_>>();
            let local = match self.raw_path_infos(&path_refs, false, false) {
                Ok(local) => Some(local),
                Err(NixAdapterError::OperationFailed) => None,
                Err(_) => return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed)),
            };
            let mut missing = Vec::new();
            for (offset, path) in chunk.iter().enumerate() {
                if let Some(local) = &local {
                    let entry = root_path_info_optional(local, path)
                        .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
                    if let Some(entry) = entry {
                        observations[chunk_start + offset] =
                            Some(CachePathObservation::hit(path.clone(), 0, entry.nar_size));
                        continue;
                    }
                }
                missing.push((chunk_start + offset, path));
            }
            if missing.is_empty() {
                continue;
            }
            if !remote_ready {
                let mut remote_ping = base_args();
                remote_ping.extend(os_args(["store", "ping", "--store", CACHE_URL]));
                self.require_success(MethodKind::PathInfo, remote_ping, SHORT_TIMEOUT)
                    .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
                remote_ready = true;
            }
            let remote_paths = missing.iter().map(|(_, path)| *path).collect::<Vec<_>>();
            let remote = match self.raw_path_infos(&remote_paths, false, true) {
                Ok(remote) => Some(remote),
                Err(NixAdapterError::OperationFailed) => None,
                Err(_) => return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed)),
            };
            let mut remote_hits = Vec::new();
            for (index, path) in missing {
                let Some((download_bytes, nar_size)) = self.remote_cache_sizes(&remote, path)?
                else {
                    observations[index] = Some(CachePathObservation::miss(path.clone()));
                    continue;
                };
                remote_hits.push((index, path, download_bytes, nar_size));
            }
            let trusted_paths = remote_hits
                .iter()
                .map(|(_, path, _, _)| *path)
                .collect::<Vec<_>>();
            if !trusted_paths.is_empty() {
                self.verify_remote_cache_trust_batch(&trusted_paths)?;
            }
            for (index, path, download_bytes, nar_size) in remote_hits {
                observations[index] = Some(CachePathObservation::hit(
                    path.clone(),
                    download_bytes,
                    nar_size,
                ));
            }
        }
        observations
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))
    }

    fn inspect_download_closures(
        &self,
        roots: &[StorePath],
    ) -> Result<Vec<CacheDownloadClosure>, BuildCacheError> {
        if roots.is_empty() {
            return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
        }
        let mut local_ping = base_args();
        local_ping.extend(os_args(["store", "ping"]));
        self.require_success(MethodKind::PathInfo, local_ping, SHORT_TIMEOUT)
            .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;

        let mut closures = Vec::with_capacity(roots.len());
        let mut remote_ready = false;
        for root in roots {
            match self.raw_path_info(root, false, false) {
                Ok(local) => {
                    let entry = root_path_info_optional(&local, root)
                        .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
                    if let Some(entry) = entry {
                        closures.push(CacheDownloadClosure::new(
                            root.clone(),
                            vec![CachePathObservation::hit(root.clone(), 0, entry.nar_size)],
                        )?);
                        continue;
                    }
                }
                Err(NixAdapterError::OperationFailed) => {}
                Err(_) => {
                    return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
                }
            }
            if !remote_ready {
                let mut remote_ping = base_args();
                remote_ping.extend(os_args(["store", "ping", "--store", CACHE_URL]));
                self.require_success(MethodKind::PathInfo, remote_ping, SHORT_TIMEOUT)
                    .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
                remote_ready = true;
            }
            // Nix expands a recursive closure before it writes path-info JSON.
            // A missing root can therefore make the recursive command fail with
            // no typed payload. Probe the root first so that an ordinary cache
            // miss remains distinct from a failure while expanding a known hit.
            let remote_root = match self.raw_path_info(root, false, true) {
                Ok(remote_root) => remote_root,
                Err(NixAdapterError::OperationFailed) => {
                    closures.push(CacheDownloadClosure::new(
                        root.clone(),
                        vec![CachePathObservation::miss(root.clone())],
                    )?);
                    continue;
                }
                Err(_) => return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed)),
            };
            if root_path_info_optional(&remote_root, root)
                .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?
                .is_none()
            {
                closures.push(CacheDownloadClosure::new(
                    root.clone(),
                    vec![CachePathObservation::miss(root.clone())],
                )?);
                continue;
            }
            let remote = self
                .raw_path_info(root, true, true)
                .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
            validate_path_info_envelope(&remote)
                .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
            if root_path_info_optional(&remote, root)
                .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?
                .is_none()
            {
                return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
            }
            let mut paths = Vec::with_capacity(remote.info.len());
            let mut remote_paths = Vec::new();
            for (name, remote_entry) in &remote.info {
                let path = store_path(name)
                    .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
                let Some(remote_entry) = remote_entry else {
                    paths.push(CachePathObservation::miss(path));
                    continue;
                };
                match self.raw_path_info(&path, false, false) {
                    Ok(local) => {
                        if let Some(local_entry) = root_path_info_optional(&local, &path)
                            .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?
                        {
                            paths.push(CachePathObservation::hit(path, 0, local_entry.nar_size));
                            continue;
                        }
                    }
                    Err(NixAdapterError::OperationFailed) => {}
                    Err(_) => {
                        return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
                    }
                }
                let signatures = signatures(&remote_entry.signatures)
                    .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
                let download_bytes = remote_entry
                    .download_size
                    .ok_or_else(|| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
                if !has_approved_cache_signature(&signatures) {
                    return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
                }
                remote_paths.push(path.clone());
                paths.push(CachePathObservation::hit(
                    path,
                    download_bytes,
                    remote_entry.nar_size,
                ));
            }
            if !remote_paths.is_empty() {
                let remote_paths = remote_paths.iter().collect::<Vec<_>>();
                self.verify_remote_cache_trust_batch(&remote_paths)?;
            }
            closures.push(CacheDownloadClosure::new(root.clone(), paths)?);
        }
        Ok(closures)
    }
}

/// Extracts and validates the `(download, nar)` sizes of one raw path info.
pub(super) fn raw_path_sizes(entry: &RawPathInfo) -> Result<(u64, u64), BuildCacheError> {
    let signatures = signatures(&entry.signatures)
        .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
    let download_bytes = entry
        .download_size
        .ok_or_else(|| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
    if !has_approved_cache_signature(&signatures) {
        return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
    }
    Ok((download_bytes, entry.nar_size))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawDerivationEnvelope {
    pub(super) version: u32,
    pub(super) derivations: BTreeMap<String, RawDerivation>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RawDerivation {
    pub(super) args: Vec<String>,
    pub(super) builder: String,
    pub(super) env: BTreeMap<String, String>,
    pub(super) inputs: RawInputs,
    pub(super) name: String,
    pub(super) outputs: BTreeMap<String, RawDerivationOutput>,
    pub(super) structured_attrs: Option<BTreeMap<String, serde_json::Value>>,
    pub(super) system: String,
    pub(super) version: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawInputs {
    pub(super) drvs: BTreeMap<String, RawInputDerivation>,
    pub(super) srcs: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(super) enum RawInputDerivation {
    Outputs(Vec<String>),
    Dynamic(RawDynamicInputDerivation),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RawDynamicInputDerivation {
    pub(super) dynamic_outputs: BTreeMap<String, serde_json::Value>,
    pub(super) outputs: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RawDerivationOutput {
    pub(super) path: Option<String>,
    pub(super) hash: Option<String>,
    pub(super) method: Option<String>,
    pub(super) hash_algo: Option<String>,
    pub(super) impure: Option<bool>,
}

pub(super) fn normalize_derivation(
    bytes: &[u8],
    request: &EvaluateDerivationRequest,
    root_name: &str,
) -> Result<DerivationPlanReport, NixAdapterError> {
    let raw: RawDerivationEnvelope = parse_json(bytes)?;
    validate_derivation_envelope(&raw)?;
    let root_raw = raw
        .derivations
        .get(root_name)
        .ok_or(NixAdapterError::OperationFailed)?;
    let root = derivation_path(root_name)?;
    let structured_attr = |name: &str| {
        root_raw
            .structured_attrs
            .as_ref()
            .and_then(|attrs| attrs.get(name))
    };
    let string_attr = |name: &str| -> Result<Option<&str>, NixAdapterError> {
        match structured_attr(name) {
            Some(serde_json::Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(NixAdapterError::OperationFailed),
            None => Ok(root_raw.env.get(name).map(String::as_str)),
        }
    };
    let structured_outputs =
        |value: &serde_json::Value| -> Result<Vec<OutputName>, NixAdapterError> {
            value
                .as_array()
                .ok_or(NixAdapterError::OperationFailed)?
                .iter()
                .map(|name| {
                    OutputName::new(name.as_str().ok_or(NixAdapterError::OperationFailed)?)
                        .map_err(|_| NixAdapterError::OperationFailed)
                })
                .collect()
        };
    let named_structured_outputs =
        |name: &str| -> Result<Option<Vec<OutputName>>, NixAdapterError> {
            structured_attr(name).map(structured_outputs).transpose()
        };
    let legacy_outputs = |name: &str| -> Result<Option<Vec<OutputName>>, NixAdapterError> {
        root_raw
            .env
            .get(name)
            .map(|outputs| {
                outputs
                    .split_whitespace()
                    .map(|name| OutputName::new(name).map_err(|_| NixAdapterError::OperationFailed))
                    .collect()
            })
            .transpose()
    };
    let meta_outputs = || -> Result<Option<Vec<OutputName>>, NixAdapterError> {
        let Some(meta) = structured_attr("meta") else {
            return Ok(None);
        };
        let meta = meta.as_object().ok_or(NixAdapterError::OperationFailed)?;
        meta.get("outputsToInstall")
            .map(structured_outputs)
            .transpose()
    };
    let outputs_to_install = match request.outputs().explicit_outputs() {
        Some(outputs) => outputs.to_vec(),
        None => meta_outputs()?
            .or(named_structured_outputs("outputsToInstall")?)
            .or(legacy_outputs("outputsToInstall")?)
            .or(named_structured_outputs("outputs")?)
            .or(legacy_outputs("outputs")?)
            .ok_or(NixAdapterError::OperationFailed)?,
    };
    let mut derivations = Vec::with_capacity(raw.derivations.len());
    for (raw_path, item) in &raw.derivations {
        let system = DerivationSystem::from_str(&item.system)?;
        let outputs = item
            .outputs
            .iter()
            .map(|(name, output)| {
                let fixed_output = validate_derivation_output(output)?;
                let path = output
                    .path
                    .as_deref()
                    .or_else(|| item.env.get(name).map(String::as_str))
                    .ok_or(NixAdapterError::OperationFailed)?;
                Ok((
                    OutputName::new(name).map_err(|_| NixAdapterError::OperationFailed)?,
                    store_path(path)?,
                    fixed_output,
                ))
            })
            .collect::<Result<Vec<_>, NixAdapterError>>()?;
        let fixed_output = outputs.iter().any(|(_, _, fixed)| *fixed);
        let outputs = outputs
            .into_iter()
            .map(|(name, path, _)| (name, path))
            .collect::<BTreeMap<_, _>>();
        let document = serde_json::to_vec(item).map_err(|_| malformed())?;
        derivations.push(EvaluatedDerivation::new(
            derivation_path(raw_path)?,
            item.name.clone(),
            system,
            outputs,
            body_digest(&document),
            fixed_output,
        )?);
    }
    let closure = serde_json::to_vec(&raw.derivations).map_err(|_| malformed())?;
    let pname = string_attr("pname")?
        .map(str::to_owned)
        .ok_or(NixAdapterError::OperationFailed)?;
    let version = string_attr("version")?
        .map(str::to_owned)
        .unwrap_or_default();
    DerivationPlanReport::new(
        raw.version,
        root,
        outputs_to_install,
        derivations,
        body_digest(&closure),
        pname,
        pkg_core::PackageVersion::new(version),
    )
}

pub(super) fn validate_derivation_output(
    output: &RawDerivationOutput,
) -> Result<bool, NixAdapterError> {
    match (
        output.path.is_some(),
        output.hash.as_deref(),
        output.method.as_deref(),
        output.hash_algo.as_deref(),
        output.impure,
    ) {
        (true, None, None, None, None) => Ok(false),
        (false, Some(hash), Some(method), None, None)
            if valid_hash(hash) && valid_ca_method(method) =>
        {
            Ok(true)
        }
        (false, None, Some(method), Some(algorithm), None)
            if valid_ca_method(method) && valid_hash_algorithm(algorithm) =>
        {
            Err(NixAdapterError::OperationFailed)
        }
        (false, None, Some(method), Some(algorithm), Some(true))
            if valid_ca_method(method) && valid_hash_algorithm(algorithm) =>
        {
            Err(NixAdapterError::PermissionDenied)
        }
        (false, None, None, None, None) => Err(NixAdapterError::OperationFailed),
        _ => Err(NixAdapterError::OperationFailed),
    }
}

pub(super) fn valid_ca_method(value: &str) -> bool {
    matches!(value, "flat" | "nar" | "text" | "git")
}

pub(super) fn valid_hash_algorithm(value: &str) -> bool {
    matches!(value, "blake3" | "md5" | "sha1" | "sha256" | "sha512")
}

pub(super) fn valid_hash(value: &str) -> bool {
    value
        .split_once('-')
        .is_some_and(|(algorithm, digest)| valid_hash_algorithm(algorithm) && !digest.is_empty())
}

pub(super) fn single_derivation_name(bytes: &[u8]) -> Result<String, NixAdapterError> {
    let raw: RawDerivationEnvelope = parse_json(bytes)?;
    validate_derivation_envelope(&raw)?;
    if raw.derivations.len() != 1 {
        return Err(NixAdapterError::OperationFailed);
    }
    raw.derivations
        .into_keys()
        .next()
        .ok_or(NixAdapterError::OperationFailed)
}

pub(super) fn validate_derivation_envelope(
    raw: &RawDerivationEnvelope,
) -> Result<(), NixAdapterError> {
    if raw.version != 4 || raw.derivations.values().any(|item| item.version != 4) {
        return Err(NixAdapterError::UnsupportedUpstreamFormat {
            command: MethodKind::EvaluateDerivation,
            observed: raw.version,
        });
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RawPathInfoEnvelope {
    pub(super) version: u32,
    pub(super) store_dir: String,
    pub(super) info: BTreeMap<String, Option<RawPathInfo>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RawPathInfo {
    pub(super) ca: Option<RawContentAddress>,
    pub(super) compression: Option<String>,
    pub(super) deriver: Option<String>,
    pub(super) download_hash: Option<String>,
    pub(super) download_size: Option<u64>,
    pub(super) nar_hash: String,
    pub(super) nar_size: u64,
    pub(super) references: Vec<String>,
    #[serde(rename = "registrationTime")]
    pub(super) _registration_time: Option<u64>,
    pub(super) signatures: Vec<String>,
    pub(super) store_dir: String,
    pub(super) ultimate: bool,
    pub(super) url: Option<String>,
    pub(super) version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawContentAddress {
    pub(super) hash: String,
    pub(super) method: String,
}

pub(super) fn normalize_path_info(
    raw: &RawPathInfoEnvelope,
    requested: &StorePath,
) -> Result<PathInfoReport, NixAdapterError> {
    validate_path_info_envelope(raw)?;
    let root = root_path_info(raw, requested)?;
    let closure_size = raw.info.values().flatten().try_fold(0_u64, |total, item| {
        total
            .checked_add(item.nar_size)
            .ok_or(NixAdapterError::OperationFailed)
    })?;
    let references = root
        .references
        .iter()
        .map(|reference| store_path(reference))
        .filter(|result| result.as_ref() != Ok(requested))
        .collect::<Result<Vec<_>, _>>()?;
    let deriver = root.deriver.as_deref().map(derivation_path).transpose()?;
    PathInfoReport::new(
        requested.clone(),
        NarHash::new(&root.nar_hash).map_err(|_| NixAdapterError::OperationFailed)?,
        signatures(&root.signatures)?,
        references,
        deriver,
        root.nar_size,
        closure_size,
    )
}

pub(super) fn validate_path_info_envelope(
    raw: &RawPathInfoEnvelope,
) -> Result<(), NixAdapterError> {
    if raw.version != PATH_INFO_FORMAT
        || raw.store_dir != STORE_DIRECTORY
        || raw.info.values().flatten().any(|item| {
            let remote_fields = [
                item.compression.is_some(),
                item.download_hash.is_some(),
                item.download_size.is_some(),
                item.url.is_some(),
            ];
            item.version != PATH_INFO_FORMAT
                || item.store_dir != STORE_DIRECTORY
                || item.ca.as_ref().is_some_and(|ca| {
                    NarHash::new(&ca.hash).is_err() || !valid_ca_method(&ca.method)
                })
                || remote_fields
                    .iter()
                    .any(|present| *present != remote_fields[0])
        })
    {
        return Err(NixAdapterError::UnsupportedUpstreamFormat {
            command: MethodKind::PathInfo,
            observed: raw.version,
        });
    }
    Ok(())
}

pub(super) fn root_path_info<'a>(
    raw: &'a RawPathInfoEnvelope,
    requested: &StorePath,
) -> Result<&'a RawPathInfo, NixAdapterError> {
    root_path_info_optional(raw, requested)?.ok_or(NixAdapterError::OperationFailed)
}

pub(super) fn root_path_info_optional<'a>(
    raw: &'a RawPathInfoEnvelope,
    requested: &StorePath,
) -> Result<Option<&'a RawPathInfo>, NixAdapterError> {
    validate_path_info_envelope(raw)?;
    let name = requested
        .as_str()
        .strip_prefix("/nix/store/")
        .ok_or(NixAdapterError::OperationFailed)?;
    raw.info
        .get(name)
        .map(Option::as_ref)
        .ok_or(NixAdapterError::OperationFailed)
}

pub(super) fn batch_path_info_optional<'a>(
    raw: &'a RawPathInfoEnvelope,
    requested: &StorePath,
) -> Result<Option<&'a RawPathInfo>, NixAdapterError> {
    validate_path_info_envelope(raw)?;
    let name = requested
        .as_str()
        .strip_prefix("/nix/store/")
        .ok_or(NixAdapterError::OperationFailed)?;
    Ok(raw.info.get(name).and_then(Option::as_ref))
}

pub(super) fn signatures(values: &[String]) -> Result<Vec<Signature>, NixAdapterError> {
    values
        .iter()
        .map(|value| Signature::new(value).map_err(|_| NixAdapterError::TrustFailure))
        .collect()
}

pub(super) fn has_approved_cache_signature(signatures: &[Signature]) -> bool {
    signatures
        .iter()
        .any(|signature| signature.key_name() == CACHE_SIGNING_KEY_NAME)
}

pub(super) fn classify_build_provenance(
    adapter: &RealNixAdapter,
    path: &StorePath,
    ultimate: bool,
    signatures: &[Signature],
) -> Result<BuildOutputProvenance, NixAdapterError> {
    if ultimate {
        return Ok(BuildOutputProvenance::LocalBuild);
    }
    if has_approved_cache_signature(signatures)
        && verify_dimension(adapter, path, "--no-contents", 2, false)?
    {
        return Ok(BuildOutputProvenance::CacheSigned);
    }
    Err(NixAdapterError::TrustFailure)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RawBuildResult {
    pub(super) drv_path: String,
    pub(super) outputs: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) start_time: Option<u64>,
    #[serde(default)]
    pub(super) stop_time: Option<u64>,
    #[serde(default)]
    pub(super) cpu_user: Option<f64>,
    #[serde(default)]
    pub(super) cpu_system: Option<f64>,
}

pub(super) fn validate_build_metrics(result: &RawBuildResult) -> Result<(), NixAdapterError> {
    let _ = (result.start_time, result.stop_time);
    if [result.cpu_user, result.cpu_system]
        .into_iter()
        .flatten()
        .any(|seconds| !seconds.is_finite() || seconds < 0.0)
    {
        return Err(NixAdapterError::OperationFailed);
    }
    Ok(())
}

pub(super) fn expected_build_outputs(request: &BuildRequest) -> BTreeSet<(String, Option<String>)> {
    request
        .targets()
        .iter()
        .flat_map(|target| match target.outputs() {
            Some(outputs) => outputs
                .iter()
                .map(|output| {
                    (
                        target.derivation().as_str().to_owned(),
                        Some(output.as_str().to_owned()),
                    )
                })
                .collect::<Vec<_>>(),
            None => vec![(target.derivation().as_str().to_owned(), None)],
        })
        .collect()
}

pub(super) fn verify_dimension(
    adapter: &RealNixAdapter,
    path: &StorePath,
    fixed_flag: &'static str,
    failure_code: i32,
    recursive: bool,
) -> Result<bool, NixAdapterError> {
    let mut args = base_args();
    args.extend(os_args(["store", "verify", fixed_flag]));
    if recursive {
        args.push("--recursive".into());
    }
    args.push(path.as_str().into());
    let outcome = adapter.run(MethodKind::Verify, args, BUILD_TIMEOUT)?;
    match outcome.code {
        Some(0) => Ok(true),
        Some(code) if code == failure_code => Ok(false),
        _ => Err(NixAdapterError::OperationFailed),
    }
}

pub(super) fn store_path(value: &str) -> Result<StorePath, NixAdapterError> {
    let absolute = if value.starts_with('/') {
        value.to_owned()
    } else {
        format!("{STORE_DIRECTORY}/{value}")
    };
    StorePath::new(&absolute).map_err(|_| NixAdapterError::OperationFailed)
}

pub(super) fn derivation_path(value: &str) -> Result<DerivationPath, NixAdapterError> {
    DerivationPath::from_str(store_path(value)?.as_str())
        .map_err(|_| NixAdapterError::OperationFailed)
}

pub(super) fn parse_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, NixAdapterError> {
    serde_json::from_slice(bytes).map_err(|_| malformed())
}

pub(super) const fn malformed() -> NixAdapterError {
    NixAdapterError::MalformedPayload {
        kind: crate::MalformedKind::Json,
    }
}
