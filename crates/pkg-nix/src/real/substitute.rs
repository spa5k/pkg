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
                            Some(CachePathObservation::local(path.clone(), entry.nar_size));
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

        let root_refs = roots.iter().collect::<Vec<_>>();
        let mut local_sizes = self
            .local_cache_sizes(&root_refs)
            .inspect_err(|error| cache_stage_failure("root-local-sizes", *error))?;
        let mut closures = BTreeMap::new();
        let mut missing_roots = Vec::new();
        for root in roots {
            if let Some(size) = local_sizes[root.as_str()] {
                closures.insert(
                    root.as_str().to_owned(),
                    CacheDownloadClosure::new(
                        root.clone(),
                        vec![CachePathObservation::local(root.clone(), size)],
                    )?,
                );
            } else {
                missing_roots.push(root);
            }
        }
        closures.extend(
            self.remote_cache_closures(&missing_roots, &mut local_sizes)
                .inspect_err(|error| cache_stage_failure("remote-closures", *error))?,
        );
        roots
            .iter()
            .map(|root| {
                closures
                    .get(root.as_str())
                    .cloned()
                    .ok_or_else(|| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))
            })
            .collect()
    }
}

impl RealNixAdapter {
    fn remote_cache_closures(
        &self,
        roots: &[&StorePath],
        local_sizes: &mut BTreeMap<String, Option<u64>>,
    ) -> Result<BTreeMap<String, CacheDownloadClosure>, BuildCacheError> {
        let mut closures = BTreeMap::new();
        if roots.is_empty() {
            return Ok(closures);
        }
        let mut remote_ping = base_args();
        remote_ping.extend(os_args(["store", "ping", "--store", CACHE_URL]));
        self.require_success(MethodKind::PathInfo, remote_ping, SHORT_TIMEOUT)
            .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
        // A missing root can make remote path-info fail without JSON. Presence
        // selects the recursive queries; it never replaces signature checks.
        let remote_misses = self
            .cache_misses(roots, true)
            .inspect_err(|error| cache_stage_failure("remote-validity", *error))?;
        let mut cached_roots = Vec::new();
        for &root in roots {
            if remote_misses.contains(root.as_str()) {
                closures.insert(
                    root.as_str().to_owned(),
                    CacheDownloadClosure::new(
                        root.clone(),
                        vec![CachePathObservation::miss(root.clone())],
                    )?,
                );
            } else {
                cached_roots.push(root);
            }
        }
        for chunk in cached_roots.chunks(PATH_INFO_BATCH_SIZE) {
            for closure in self.remote_cache_closure_batch(chunk, local_sizes)? {
                closures.insert(closure.root().as_str().to_owned(), closure);
            }
        }
        Ok(closures)
    }

    fn remote_cache_closure_batch(
        &self,
        roots: &[&StorePath],
        local_sizes: &mut BTreeMap<String, Option<u64>>,
    ) -> Result<Vec<CacheDownloadClosure>, BuildCacheError> {
        let remote = self
            .raw_path_infos(roots, true, true)
            .map_err(|error| cache_query_failure("remote-closure", &error))?;
        let entries = cache_closure_entries(&remote)
            .inspect_err(|error| cache_stage_failure("closure-entries", *error))?;
        // Recover each root's references from the union, so unrelated roots
        // cannot inherit each other's missing dependencies.
        let members = roots
            .iter()
            .map(|root| cache_closure_members(root, &entries))
            .collect::<Result<Vec<_>, _>>()
            .inspect_err(|error| cache_stage_failure("closure-members", *error))?;
        let unknown_local = entries
            .values()
            .filter(|(path, entry)| entry.is_some() && !local_sizes.contains_key(path.as_str()))
            .map(|(path, _)| path)
            .collect::<Vec<_>>();
        local_sizes.extend(
            self.local_cache_sizes(&unknown_local)
                .inspect_err(|error| cache_stage_failure("closure-local-sizes", *error))?,
        );
        let mut observations = BTreeMap::new();
        let mut remote_paths = Vec::new();
        for (name, (path, entry)) in &entries {
            let observation = match (entry, local_sizes.get(path.as_str()).copied().flatten()) {
                (Some(_), Some(size)) => CachePathObservation::local(path.clone(), size),
                (Some(entry), None) => {
                    let (download_bytes, nar_bytes) = raw_path_sizes(entry)
                        .inspect_err(|error| cache_stage_failure("remote-sizes", *error))?;
                    remote_paths.push(path);
                    CachePathObservation::hit(path.clone(), download_bytes, nar_bytes)
                }
                (None, _) => CachePathObservation::miss(path.clone()),
            };
            observations.insert(name.clone(), observation);
        }
        for paths in remote_paths.chunks(PATH_INFO_BATCH_SIZE) {
            self.verify_remote_cache_trust_batch(paths)
                .inspect_err(|error| cache_stage_failure("signature-verification", *error))?;
        }
        roots
            .iter()
            .zip(members)
            .map(|(root, members)| {
                let paths = members
                    .iter()
                    .map(|name| observations[name].clone())
                    .collect();
                CacheDownloadClosure::new((*root).clone(), paths)
            })
            .collect()
    }

    /// The legacy validity query prints only invalid paths and succeeds for
    /// ordinary cache misses. Unlike path-info, mixed batches need no retries.
    /// Presence is not trust: recursive metadata and signature verification
    /// still have to succeed before any cached closure is returned.
    fn cache_misses(
        &self,
        paths: &[&StorePath],
        remote: bool,
    ) -> Result<BTreeSet<String>, BuildCacheError> {
        let failed = || BuildCacheError::new(BuildCacheErrorCode::ProbeFailed);
        let mut missing = BTreeSet::new();
        for chunk in paths.chunks(PATH_INFO_BATCH_SIZE) {
            let mut args = base_args();
            args.extend(os_args(["--check-validity", "--print-invalid"]));
            if remote {
                args.extend(os_args(["--store", CACHE_URL]));
            }
            args.extend(chunk.iter().map(|path| OsString::from(path.as_str())));
            let outcome = self
                .run_with_program(
                    MethodKind::PathInfo,
                    NixProgram::LegacyStore,
                    args,
                    SHORT_TIMEOUT,
                )
                .map_err(|error| cache_query_failure("validity", &error))?;
            if outcome.code != Some(0) {
                return Err(failed());
            }
            let output = std::str::from_utf8(&outcome.stdout).map_err(|_| failed())?;
            for line in output.lines() {
                let path = StorePath::new(line).map_err(|_| failed())?;
                if !chunk.contains(&&path) || !missing.insert(path.as_str().to_owned()) {
                    return Err(failed());
                }
            }
        }
        Ok(missing)
    }

    /// Batch existence and size checks. A failed metadata query uses a batched
    /// validity query; an explicit JSON null is already a definitive miss.
    fn local_cache_sizes(
        &self,
        paths: &[&StorePath],
    ) -> Result<BTreeMap<String, Option<u64>>, BuildCacheError> {
        let mut sizes = BTreeMap::new();
        for chunk in paths.chunks(PATH_INFO_BATCH_SIZE) {
            let raw = match self.raw_path_infos(chunk, false, false) {
                Ok(raw) => raw,
                Err(NixAdapterError::OperationFailed) => {
                    sizes.extend(self.local_sizes_after_failed_batch(chunk)?);
                    continue;
                }
                Err(error) => return Err(cache_query_failure("local-metadata", &error)),
            };
            validate_path_info_envelope(&raw)
                .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
            for path in chunk {
                let entry = root_path_info_optional(&raw, path)
                    .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?;
                sizes.insert(path.as_str().to_owned(), entry.map(|entry| entry.nar_size));
            }
        }
        Ok(sizes)
    }

    fn local_sizes_after_failed_batch(
        &self,
        paths: &[&StorePath],
    ) -> Result<BTreeMap<String, Option<u64>>, BuildCacheError> {
        let failed = || BuildCacheError::new(BuildCacheErrorCode::ProbeFailed);
        let missing = self.cache_misses(paths, false)?;
        let mut sizes = missing
            .iter()
            .map(|path| (path.clone(), None))
            .collect::<BTreeMap<_, _>>();
        let present = paths
            .iter()
            .copied()
            .filter(|path| !missing.contains(path.as_str()))
            .collect::<Vec<_>>();
        if present.is_empty() {
            return Ok(sizes);
        }
        let raw = self
            .raw_path_infos(&present, false, false)
            .map_err(|error| cache_query_failure("local-present-metadata", &error))?;
        validate_path_info_envelope(&raw).map_err(|_| failed())?;
        for path in present {
            let entry = root_path_info(&raw, path).map_err(|_| failed())?;
            sizes.insert(path.as_str().to_owned(), Some(entry.nar_size));
        }
        Ok(sizes)
    }
}

fn cache_query_failure(stage: &'static str, error: &NixAdapterError) -> BuildCacheError {
    eprintln!(
        "pkg cache probe failed: stage={stage} code={:?}",
        error.code()
    );
    BuildCacheError::new(BuildCacheErrorCode::ProbeFailed)
}

fn cache_stage_failure(stage: &'static str, error: BuildCacheError) {
    eprintln!(
        "pkg cache probe failed: stage={stage} code={:?}",
        error.code()
    );
}

type CacheClosureEntries<'a> = BTreeMap<String, (StorePath, Option<&'a RawPathInfo>)>;

fn cache_closure_entries(
    remote: &RawPathInfoEnvelope,
) -> Result<CacheClosureEntries<'_>, BuildCacheError> {
    let failed = || BuildCacheError::new(BuildCacheErrorCode::ProbeFailed);
    validate_path_info_envelope(remote).map_err(|_| failed())?;
    let mut entries = BTreeMap::new();
    for (name, entry) in &remote.info {
        let path = store_path(name).map_err(|_| failed())?;
        if entries
            .insert(path.as_str().to_owned(), (path, entry.as_ref()))
            .is_some()
        {
            return Err(failed());
        }
    }
    Ok(entries)
}

/// Follow only references reachable from this root in the recursive union.
fn cache_closure_members(
    root: &StorePath,
    entries: &CacheClosureEntries<'_>,
) -> Result<BTreeSet<String>, BuildCacheError> {
    let failed = || BuildCacheError::new(BuildCacheErrorCode::ProbeFailed);
    if !entries
        .get(root.as_str())
        .is_some_and(|(_, entry)| entry.is_some())
    {
        return Err(failed());
    }
    let mut pending = vec![root.as_str().to_owned()];
    let mut members = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !members.insert(name.clone()) {
            continue;
        }
        if members.len() > crate::build_cache::MAX_CACHE_PATHS {
            return Err(failed());
        }
        let (_, entry) = entries.get(&name).ok_or_else(failed)?;
        if let Some(entry) = entry {
            for reference in &entry.references {
                let path = store_path(reference).map_err(|_| failed())?;
                if !members.contains(path.as_str()) {
                    pending.push(path.as_str().to_owned());
                }
            }
        }
    }
    Ok(members)
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
        derivations.push(
            EvaluatedDerivation::new(
                derivation_path(raw_path)?,
                item.name.clone(),
                system,
                outputs,
                body_digest(&document),
                fixed_output,
            )?
            .with_input_outputs(derivation_input_outputs(item, &raw.derivations)?)?,
        );
    }
    let closure = serde_json::to_vec(&raw.derivations).map_err(|_| malformed())?;
    let pname = string_attr("pname")?.unwrap_or(&root_raw.name).to_owned();
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

fn derivation_input_outputs(
    derivation: &RawDerivation,
    closure: &BTreeMap<String, RawDerivation>,
) -> Result<Vec<StorePath>, NixAdapterError> {
    let mut paths = BTreeMap::new();
    for (name, input) in &derivation.inputs.drvs {
        let dependency = closure.get(name).ok_or(NixAdapterError::OperationFailed)?;
        let outputs = match input {
            RawInputDerivation::Outputs(outputs) => outputs,
            RawInputDerivation::Dynamic(input) if input.dynamic_outputs.is_empty() => {
                &input.outputs
            }
            RawInputDerivation::Dynamic(_) => return Err(NixAdapterError::OperationFailed),
        };
        for output in outputs {
            let value = dependency
                .outputs
                .get(output)
                .ok_or(NixAdapterError::OperationFailed)?;
            let path = value
                .path
                .as_deref()
                .or_else(|| dependency.env.get(output).map(String::as_str))
                .ok_or(NixAdapterError::OperationFailed)?;
            let path = store_path(path)?;
            paths.insert(path.as_str().to_owned(), path);
        }
    }
    Ok(paths.into_values().collect())
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
pub struct RawPathInfoEnvelope {
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
