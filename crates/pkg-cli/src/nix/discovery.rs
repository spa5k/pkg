//! Small metadata projections through the external Nix evaluator.

use super::{Nix, NixError, SearchMeta, process};

/// One evaluated path, containing either metadata or recursive child names.
#[derive(Debug, serde::Deserialize)]
pub struct MetadataNode {
    /// Full attribute path components, including the output root and system.
    pub path: Vec<String>,
    /// The evaluated value at this path.
    #[serde(flatten)]
    pub value: MetadataValue,
}

/// The possible results of inspecting one native attribute.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MetadataValue {
    /// A derivation with projected metadata.
    Package {
        /// The package name, version, and description.
        meta: SearchMeta,
    },
    /// A recursive set to visit.
    Children {
        /// Attribute names under this set.
        children: Vec<String>,
    },
    /// A value outside the package traversal.
    Skip,
    /// A legacy attribute whose evaluation failed.
    Error,
}

pub(super) fn nix_json(value: &impl serde::Serialize) -> Result<String, NixError> {
    let json = serde_json::to_string(value).map_err(|error| NixError::Json {
        what: "metadata arguments",
        source: error,
    })?;
    serde_json::to_string(&json)
        .map(|literal| literal.replace("${", "\\${"))
        .map_err(|error| NixError::Json {
            what: "metadata arguments",
            source: error,
        })
}

impl Nix {
    pub(crate) fn exact_metadata(
        &self,
        reference: &str,
        paths: &[Vec<String>],
    ) -> Result<Vec<MetadataNode>, NixError> {
        let reference = self.exact_reference(reference)?;
        let reference = nix_json(&reference)?;
        let paths = nix_json(&paths)?;
        let expression = format!(
            r#"let
  flake = builtins.getFlake (builtins.fromJSON {reference});
  paths = builtins.fromJSON {paths};
  inspect = path:
    let
      value = builtins.foldl' (v: name:
        if builtins.isAttrs v && builtins.hasAttr name v
        then builtins.getAttr name v else null) flake path;
    in if builtins.isAttrs value && value ? type && !builtins.isString value.type then
      throw "package type must be a string"
    else if builtins.isAttrs value && (value.type or null) == "derivation" then
      let name = builtins.parseDrvName value.name;
      in {{ inherit path; kind = "package"; meta = {{ pname = name.name; version = name.version; description = builtins.replaceStrings [ "\n" ] [ " " ] (value.meta.description or ""); }}; }}
    else {{ inherit path; kind = "skip"; }};
in map inspect paths"#
        );
        let args = ["eval", "--json", "--expr", &expression];
        match self.discovery_json("exact package metadata", &args, "20") {
            Err(NixError::ResourceLimit(_)) => {
                self.discovery_json("exact package metadata", &args, "100")
            }
            result => result,
        }
    }

    fn exact_reference(&self, reference: &str) -> Result<String, NixError> {
        if !reference.starts_with("path:") {
            return Ok(reference.to_string());
        }
        // Path metadata can omit narHash. Pin its immutable store copy for
        // getFlake rather than permitting mutable paths with --impure.
        let metadata: serde_json::Value = self.json(
            "exact source identity",
            &["flake", "metadata", reference, "--json"],
        )?;
        let path = metadata["path"].as_str().ok_or_else(|| NixError::Decode {
            what: "exact source identity",
            detail: String::from("flake metadata has no store path"),
        })?;
        let hash = self.capture(&["hash", "path", "--sri", path])?;
        let hash = hash.trim();
        if !hash.starts_with("sha256-") || hash.contains(char::is_whitespace) {
            return Err(NixError::Decode {
                what: "exact source hash",
                detail: String::from("Nix did not return a SHA-256 SRI hash"),
            });
        }
        let encoded = hash
            .replace('+', "%2B")
            .replace('/', "%2F")
            .replace('=', "%3D");
        Ok(format!(
            "path:{}?narHash={encoded}",
            super::encode_path_reference(std::path::Path::new(path))
        ))
    }

    /// Evaluate at most one batch of names and descriptions with a 512 MiB RSS limit.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn metadata_batch(
        &self,
        reference: &str,
        system: &str,
        root: &str,
        paths: &[Vec<String>],
    ) -> Result<Vec<MetadataNode>, NixError> {
        match self.metadata_batch_with_gc(reference, system, root, paths, "20") {
            Err(NixError::ResourceLimit(_)) if paths.len() == 1 => {
                self.metadata_batch_with_gc(reference, system, root, paths, "100")
            }
            result => result,
        }
    }

    fn metadata_batch_with_gc(
        &self,
        reference: &str,
        system: &str,
        root: &str,
        paths: &[Vec<String>],
        divisor: &str,
    ) -> Result<Vec<MetadataNode>, NixError> {
        let paths = nix_json(&paths)?;
        let legacy = if root == "legacyPackages" {
            "true"
        } else {
            "false"
        };
        let apply = format!(
            r#"root:
let
  paths = builtins.fromJSON {paths};
  legacy = {legacy};
  inspect = path:
    let
      value = builtins.foldl' (v: name: builtins.getAttr name v) root (builtins.tail (builtins.tail path));
      row = if builtins.isAttrs value && value ? type && !builtins.isString value.type then
        throw "package type must be a string"
      else if builtins.isAttrs value && (value.type or null) == "derivation" then
        let name = builtins.parseDrvName value.name;
        in {{ inherit path; kind = "package"; meta = {{ pname = name.name; version = name.version; description = builtins.replaceStrings [ "\n" ] [ " " ] (value.meta.description or ""); }}; }}
      else if builtins.length path == 2 || (builtins.isAttrs value && legacy && (value.recurseForDerivations or false)) then
        {{ inherit path; kind = "children"; children = builtins.attrNames value; }}
      else {{ inherit path; kind = "skip"; }};
      attempt = builtins.tryEval (builtins.deepSeq row row);
    in if !legacy then row else if attempt.success then attempt.value else {{ inherit path; kind = "error"; }};
in map inspect paths"#
        );
        let installable = format!("{reference}#{root}.{system}");
        self.discovery_json(
            "metadata batch",
            &["eval", "--json", &installable, "--apply", &apply],
            divisor,
        )
    }

    pub(super) fn discovery_json<T: serde::de::DeserializeOwned>(
        &self,
        what: &'static str,
        args: &[&str],
        divisor: &str,
    ) -> Result<T, NixError> {
        let mut bounded_args = vec![
            "--option",
            "eval-attrset-update-layer-rhs-threshold",
            "65536",
        ];
        bounded_args.extend_from_slice(args);
        let command_line = Self::argv(&bounded_args);
        if self.verbose {
            eprintln!("pkg: nix {}", command_line.join(" "));
        }
        let mut command = std::process::Command::new(&self.executable);
        command.args(command_line);
        command.env("GC_INITIAL_HEAP_SIZE", "8388608");
        command.env("GC_FREE_SPACE_DIVISOR", divisor);
        command.env("MIMALLOC_PURGE_DELAY", "0");
        if self.no_color {
            command.env("NO_COLOR", "1");
        }
        let reaped =
            process::run_child_bounded(command, 512 * 1024 * 1024).map_err(NixError::Spawn)?;
        Self::finish(args, &reaped)?;
        serde_json::from_slice(&reaped.output.stdout).map_err(|error| NixError::Json {
            what,
            source: error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_rows_require_the_payload_for_their_kind() {
        for row in [
            r#"{"path":["packages"],"kind":"package"}"#,
            r#"{"path":["packages"],"kind":"children"}"#,
            r#"{"path":["packages"],"kind":"unknown"}"#,
        ] {
            assert!(serde_json::from_str::<MetadataNode>(row).is_err(), "{row}");
        }
        let row: MetadataNode =
            serde_json::from_str(r#"{"path":["packages"],"kind":"children","children":["hello"]}"#)
                .unwrap();
        assert!(matches!(row.value, MetadataValue::Children { children } if children == ["hello"]));
    }
}
