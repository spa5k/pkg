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
        /// The main command, from `meta.mainProgram`, when declared. Search
        /// data reads it only for top-level packages: in some nested sets,
        /// such as NixOS tests, it evaluates a whole system.
        #[serde(default, rename = "mainProgram")]
        command: Option<String>,
        /// Why Nixpkgs cannot install the package on this system:
        /// `unsupported-platform`, `unfree`, `broken`, or `insecure`. Search
        /// data reads it only for top-level packages, as for the command.
        #[serde(default)]
        unavailable: Option<String>,
    },
    /// A recursive set to visit.
    Children {
        /// Attribute names under this set.
        children: Vec<String>,
    },
    /// A value outside the package traversal.
    Skip,
    /// The requested output root or system is absent from this flake.
    Absent,
    /// A legacy attribute whose evaluation failed.
    Error,
}

/// A unique derivation whose output paths were evaluated.
#[derive(Clone, serde::Deserialize)]
pub struct OutputMatch {
    /// The full matched native attribute.
    pub(crate) attribute: String,
    /// Store paths selected by the request.
    pub(crate) paths: Vec<String>,
}

/// The expression that inspects one batch of attribute paths.
fn metadata_expression(
    reference: &str,
    system: &str,
    root: &str,
    paths: &[Vec<String>],
) -> Result<String, NixError> {
    let legacy = if root == "legacyPackages" {
        "true"
    } else {
        "false"
    };
    let reference = nix_json(&reference)?;
    let system = nix_json(&system)?;
    let root = nix_json(&root)?;
    let paths = nix_json(&paths)?;
    let expression = format!(
        r#"let
  flake = builtins.getFlake (builtins.fromJSON {reference});
  system = builtins.fromJSON {system};
  rootName = builtins.fromJSON {root};
  paths = builtins.fromJSON {paths};
  legacy = {legacy};
  absent = path: {{ inherit path; kind = "absent"; }};
  inspect = root: path:
let
  value = builtins.foldl' (v: name: builtins.getAttr name v) root (builtins.tail (builtins.tail path));
  row = if builtins.isAttrs value && value ? type && !builtins.isString value.type then
throw "package type must be a string"
  else if builtins.isAttrs value && (value.type or null) == "derivation" then
let name = builtins.parseDrvName value.name;
in {{ inherit path; kind = "package"; meta = {{ pname = name.name; version = name.version; description = builtins.replaceStrings [ "\n" ] [ " " ] (value.meta.description or ""); }}; mainProgram = if builtins.length path != 3 then null else let main = builtins.tryEval (value.meta.mainProgram or null); in if main.success && builtins.isString main.value then main.value else null; unavailable = if builtins.length path != 3 then null else let flag = name: let checked = builtins.tryEval (value.meta.${{name}} or false); in checked.success && checked.value == true; in if flag "unsupported" then "unsupported-platform" else if flag "unfree" then "unfree" else if flag "broken" then "broken" else if flag "insecure" then "insecure" else null; }}
  else if builtins.length path == 2 || (builtins.isAttrs value && legacy && (value.recurseForDerivations or false)) then
{{ inherit path; kind = "children"; children = builtins.attrNames value; }}
  else {{ inherit path; kind = "skip"; }};
  attempt = builtins.tryEval (builtins.deepSeq row row);
in if !legacy then row else if attempt.success then attempt.value else {{ inherit path; kind = "error"; }};
in if !builtins.isAttrs flake || !builtins.hasAttr rootName flake then
  [ (absent [ rootName system ]) ]
else let
  output = builtins.getAttr rootName flake;
in if !builtins.isAttrs output then
  throw ("Nix output root " + rootName + " must be an attribute set")
else if !builtins.hasAttr system output then
  [ (absent [ rootName system ]) ]
else let
  selected = builtins.getAttr system output;
in if !builtins.isAttrs selected then
  throw ("Nix output system " + rootName + "." + system + " must be an attribute set")
else map (inspect selected) paths"#
    );
    Ok(expression)
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
    /// Check install outputs against one locked source in bounded batches.
    /// Ambiguous or oversized requests return no fast result.
    pub(crate) fn install_outputs(
        &self,
        reference: &str,
        system: &str,
        requests: &[Vec<String>],
    ) -> Result<Vec<Option<OutputMatch>>, NixError> {
        let identity = self.source_identity(reference)?;
        let Some(locked) = identity.locked_url else {
            return Ok(vec![None; requests.len()]);
        };
        let reference = nix_json(&locked)?;
        let system = nix_json(&system)?;
        let query = include_str!("install-outputs.nix");
        let mut matches = Vec::with_capacity(requests.len());
        for chunk in requests.chunks(128) {
            let requests = nix_json(&chunk)?;
            if requests.len() > 64 * 1024 {
                matches.extend(vec![None; chunk.len()]);
                continue;
            }
            let expression = format!(
                "({query}) {{ reference = builtins.fromJSON {reference}; system = builtins.fromJSON {system}; requests = builtins.fromJSON {requests}; }}"
            );
            let rows: Vec<Option<OutputMatch>> = self.discovery_json(
                "batch install outputs",
                &["eval", "--json", "--expr", &expression],
                "20",
            )?;
            if rows.len() != chunk.len() {
                return Err(NixError::Decode {
                    what: "batch install outputs",
                    detail: String::from("result count does not match the request count"),
                });
            }
            matches.extend(rows);
        }
        Ok(matches)
    }

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
      in {{ inherit path; kind = "package"; meta = {{ pname = name.name; version = name.version; description = builtins.replaceStrings [ "\n" ] [ " " ] (value.meta.description or ""); }}; mainProgram = let main = builtins.tryEval (value.meta.mainProgram or null); in if main.success && builtins.isString main.value then main.value else null; }}
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
            &["flake", "metadata", "--json", "--", reference],
        )?;
        let path = if let Some(path) = metadata.get("path") {
            path.as_str()
                .ok_or_else(|| NixError::Decode {
                    what: "exact source identity",
                    detail: String::from("flake metadata path has an invalid type"),
                })?
                .to_string()
        } else {
            let archive: serde_json::Value = self.json(
                "exact source archive",
                &["flake", "archive", "--json", reference],
            )?;
            let path = archive
                .get("path")
                .and_then(serde_json::Value::as_str)
                .filter(|path| !path.is_empty() && std::path::Path::new(path).is_absolute())
                .ok_or_else(|| NixError::Decode {
                    what: "exact source archive",
                    detail: String::from("flake archive has no valid absolute source path"),
                })?;
            path.to_string()
        };
        let hash = self.capture(&["hash", "path", "--sri", &path])?;
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
        let mut pinned = format!(
            "path:{}?narHash={encoded}",
            super::encode_path_reference(std::path::Path::new(&path))
        );
        if let Some(dir) = metadata["locked"]["dir"]
            .as_str()
            .or_else(|| metadata["original"]["dir"].as_str())
            .filter(|dir| !dir.is_empty())
        {
            pinned.push_str("&dir=");
            pinned.push_str(&super::encode_path_reference(std::path::Path::new(dir)));
        }
        for key in ["lastModified", "rev", "revCount"] {
            let Some(value) = metadata["locked"].get(key) else {
                continue;
            };
            let value = if key == "rev" {
                value.as_str().map(str::to_string)
            } else {
                value.as_u64().map(|value| value.to_string())
            }
            .ok_or_else(|| NixError::Decode {
                what: "exact source identity",
                detail: format!("locked {key} has an invalid type"),
            })?;
            pinned.push('&');
            pinned.push_str(key);
            pinned.push('=');
            pinned.push_str(&super::encode_path_reference(std::path::Path::new(&value)));
        }
        Ok(pinned)
    }

    /// Evaluate names and descriptions in bounded requests with a 512 MiB RSS limit.
    /// # Errors
    ///
    /// Returns an error if argument encoding, child setup, stream reading, or reaping fails,
    /// or one encoded path exceeds the argument limit.
    /// Also fails when evaluation is interrupted, exceeds its resource budget,
    /// exits unsuccessfully, or returns invalid metadata JSON.
    pub fn metadata_batch(
        &self,
        reference: &str,
        system: &str,
        root: &str,
        paths: &[Vec<String>],
    ) -> Result<Vec<MetadataNode>, NixError> {
        if nix_json(&paths)?.len() > 64 * 1024 {
            if paths.len() == 1 {
                return Err(NixError::Decode {
                    what: "metadata arguments",
                    detail: String::from("one encoded path exceeds the 64 KiB argument limit"),
                });
            }
            let (left, right) = paths.split_at(paths.len() / 2);
            let mut nodes = self.metadata_batch(reference, system, root, left)?;
            nodes.extend(self.metadata_batch(reference, system, root, right)?);
            return Ok(nodes);
        }
        match self.metadata_batch_with_gc(reference, system, root, paths, "20") {
            Err(NixError::ResourceLimit(_)) if paths.len() == 1 => {
                self.metadata_batch_with_gc(reference, system, root, paths, "100")
            }
            result => result,
        }
    }

    /// Evaluate several metadata batches at the same time.
    ///
    /// Each batch is one `nix eval` with the memory budget of
    /// [`Self::metadata_batch`]. Each result has its own error. A batch that
    /// fails here does not split; the caller can retry it alone.
    /// # Errors
    ///
    /// Returns an error if argument encoding or signal setup fails.
    pub fn metadata_batches(
        &self,
        reference: &str,
        system: &str,
        root: &str,
        batches: &[&[Vec<String>]],
    ) -> Result<Vec<Result<Vec<MetadataNode>, NixError>>, NixError> {
        let expressions = batches
            .iter()
            .map(|paths| metadata_expression(reference, system, root, paths))
            .collect::<Result<Vec<_>, _>>()?;
        let args: Vec<[&str; 4]> = expressions
            .iter()
            .map(|expression| ["eval", "--json", "--expr", expression])
            .collect();
        let commands = args
            .iter()
            .map(|args| self.discovery_command(args, "20"))
            .collect();
        let results = process::run_children(commands, Some(DISCOVERY_LIMITS))
            .map_err(|error| self.diagnose(NixError::Process(error)))?;
        Ok(results
            .into_iter()
            .zip(&args)
            .map(|(result, args)| {
                let reaped = result.map_err(|error| self.diagnose(NixError::Process(error)))?;
                self.discovery_output("metadata batch", args, &reaped)
            })
            .collect())
    }

    fn metadata_batch_with_gc(
        &self,
        reference: &str,
        system: &str,
        root: &str,
        paths: &[Vec<String>],
        divisor: &str,
    ) -> Result<Vec<MetadataNode>, NixError> {
        let expression = metadata_expression(reference, system, root, paths)?;
        self.discovery_json(
            "metadata batch",
            &["eval", "--json", "--expr", &expression],
            divisor,
        )
    }

    pub(super) fn discovery_json<T: serde::de::DeserializeOwned>(
        &self,
        what: &'static str,
        args: &[&str],
        divisor: &str,
    ) -> Result<T, NixError> {
        let reaped = process::run_child(
            self.discovery_command(args, divisor),
            process::IoMode::Capture,
            Some(DISCOVERY_LIMITS),
            None,
        )
        .map_err(|error| self.diagnose(NixError::Process(error)))?;
        self.discovery_output(what, args, &reaped)
    }

    fn discovery_command(&self, args: &[&str], divisor: &str) -> std::process::Command {
        let mut bounded_args = vec![
            "--option",
            "eval-attrset-update-layer-rhs-threshold",
            "65536",
        ];
        bounded_args.extend_from_slice(args);
        let mut command = self.command(&bounded_args);
        command.env("GC_INITIAL_HEAP_SIZE", "8388608");
        command.env("GC_FREE_SPACE_DIVISOR", divisor);
        command.env("MIMALLOC_PURGE_DELAY", "0");
        command
    }

    fn discovery_output<T: serde::de::DeserializeOwned>(
        &self,
        what: &'static str,
        args: &[&str],
        reaped: &process::Reaped,
    ) -> Result<T, NixError> {
        self.finish(args, reaped)?;
        serde_json::from_slice(&reaped.output.stdout).map_err(|error| NixError::Json {
            what,
            source: error,
        })
    }
}

/// The bounds of one discovery evaluation: 512 MiB RSS, two minutes, and
/// 16 MiB of output.
const DISCOVERY_LIMITS: process::ChildLimits = process::ChildLimits {
    resident_bytes: Some(512 * 1024 * 1024),
    wall_time: std::time::Duration::from_secs(120),
    output_bytes: 16 * 1024 * 1024,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_runtime(cases: &str) -> (tempfile::TempDir, Nix) {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("nix");
        let script = [
            "#!/bin/sh\ncase \"$*\" in\n",
            "*--version*) echo 'nix (Nix) 2.35.2';;\n",
            cases,
            "\n*) exit 9;;\nesac\n",
        ]
        .concat();
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let nix = crate::nix::discover(executable.to_str()).unwrap();
        (temp, nix)
    }

    #[cfg(unix)]
    #[test]
    fn metadata_batches_bound_arguments_without_losing_paths() {
        let capture = tempfile::tempdir().unwrap();
        let calls = capture.path().join("expressions");
        let cases = format!(
            r#"*'eval --json --expr'*)
  for argument in "$@"; do
    case "$argument" in
      'let'*)
        [ "${{#argument}}" -le 70000 ] || exit 88
        printf '%s\000' "$argument" >> '{}';;
    esac
  done
  echo '[]';;"#,
            calls.display()
        );
        let (_runtime, nix) = fake_runtime(&cases);
        let paths: Vec<_> = (0..200)
            .map(|index| {
                vec![
                    String::from("legacyPackages"),
                    String::from("x86_64-linux"),
                    format!("package-{index}-{}", "x".repeat(512)),
                ]
            })
            .collect();
        assert!(
            nix.metadata_batch(
                "github:example/packages",
                "x86_64-linux",
                "legacyPackages",
                &paths,
            )
            .is_ok(),
            "all child arguments stay within their limit"
        );
        let expressions = std::fs::read_to_string(&calls).unwrap();
        let mut evaluated = Vec::new();
        let mut children = 0;
        for expression in expressions.split('\0').filter(|value| !value.is_empty()) {
            children += 1;
            let literal = expression
                .split_once("paths = builtins.fromJSON ")
                .unwrap()
                .1
                .split_once(';')
                .unwrap()
                .0;
            let json: String = serde_json::from_str(literal).unwrap();
            let chunk: Vec<Vec<String>> = serde_json::from_str(&json).unwrap();
            evaluated.extend(chunk);
        }
        assert!(children > 1, "the input requires multiple bounded children");
        assert_eq!(evaluated, paths, "each path is evaluated once, in order");
        let oversized = [vec!["x".repeat(64 * 1024)]];
        assert!(matches!(
            nix.metadata_batch(
                "github:example/packages",
                "x86_64-linux",
                "legacyPackages",
                &oversized,
            ),
            Err(NixError::Decode {
                what: "metadata arguments",
                ..
            })
        ));
        assert_eq!(std::fs::read(calls).unwrap().len(), expressions.len());
    }

    #[cfg(unix)]
    #[test]
    fn exact_path_reference_preserves_native_source_identity() {
        let (_temp, nix) = fake_runtime(
            r#"*'flake metadata'*) echo '{"path":"/nix/store/fixture-source","locked":{"dir":"sub flake?#","lastModified":1234567890,"rev":"0123456789abcdef0123456789abcdef01234567","revCount":7}}';;
*'hash path'*) echo 'sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=';;"#,
        );
        let reference = nix
            .exact_reference("path:/fixture?dir=sub%20flake%3F%23")
            .unwrap();
        assert_eq!(
            reference,
            "path:/nix/store/fixture-source?narHash=sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA%3D&dir=sub%20flake%3F%23&lastModified=1234567890&rev=0123456789abcdef0123456789abcdef01234567&revCount=7"
        );
    }

    #[cfg(unix)]
    #[test]
    fn exact_path_reference_archives_when_metadata_omits_path() {
        let (_temp, nix) = fake_runtime(
            r#"*'flake metadata'*) echo '{"locked":{"dir":"sub flake?#","lastModified":1234567890,"rev":"0123456789abcdef0123456789abcdef01234567","revCount":7}}';;
*'flake archive'*) echo '{"inputs":{},"path":"/alternate/store/fixture-archive-source"}';;
*'hash path --sri /alternate/store/fixture-archive-source'*) echo 'sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=';;"#,
        );

        let reference = nix
            .exact_reference("path:/fixture?dir=sub%20flake%3F%23")
            .unwrap();

        assert_eq!(
            reference,
            "path:/alternate/store/fixture-archive-source?narHash=sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA%3D&dir=sub%20flake%3F%23&lastModified=1234567890&rev=0123456789abcdef0123456789abcdef01234567&revCount=7"
        );
    }

    #[cfg(unix)]
    #[test]
    fn exact_path_reference_rejects_wrong_type_metadata_path_without_archiving() {
        let (_temp, nix) = fake_runtime(
            r#"*'flake metadata'*) echo '{"path":7,"locked":{}}';;
*'flake archive'*) echo '{"path":"/nix/store/must-not-be-used"}';;"#,
        );

        let result = nix.exact_reference("path:/fixture");

        assert!(matches!(
            result,
            Err(NixError::Decode {
                what: "exact source identity",
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn exact_path_reference_rejects_malformed_archive_paths() {
        for archive_json in [
            "{}",
            r#"{"path":7}"#,
            r#"{"path":""}"#,
            r#"{"path":"relative/store/source"}"#,
        ] {
            let cases = format!(
                "*'flake metadata'*) echo '{{\"locked\":{{}}}}';;\n*'flake archive'*) echo '{archive_json}';;\n*'hash path'*) exit 8;;"
            );
            let (_temp, nix) = fake_runtime(&cases);

            let result = nix.exact_reference("path:/fixture");

            assert!(
                matches!(
                    result,
                    Err(NixError::Decode {
                        what: "exact source archive",
                        ..
                    })
                ),
                "archive JSON must be rejected: {archive_json}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn exact_path_reference_preserves_archive_interruption() {
        let (_temp, nix) = fake_runtime(
            r#"*'flake metadata'*) echo '{"locked":{}}';;
*'flake archive'*) kill -TERM $$;;"#,
        );

        let result = nix.exact_reference("path:/fixture");

        assert!(matches!(result, Err(NixError::Interrupted { .. })));
    }

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
