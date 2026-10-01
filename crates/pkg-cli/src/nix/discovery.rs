//! Small metadata projections through the external Nix evaluator.

use super::{Nix, NixError, SearchMeta, process};

/// One evaluated path, containing either metadata or recursive child names.
#[derive(Debug, serde::Deserialize)]
pub struct MetadataNode {
    /// Full attribute path components, including the output root and system.
    pub path: Vec<String>,
    /// `package`, `children`, `skip`, or an ignored legacy `error`.
    pub kind: String,
    /// Package metadata, only for a derivation.
    #[serde(default)]
    pub meta: Option<SearchMeta>,
    /// Attribute names to visit under a recursive set.
    #[serde(default)]
    pub children: Vec<String>,
}

pub(super) fn nix_json(value: &impl serde::Serialize) -> Result<String, NixError> {
    let json = serde_json::to_string(value).map_err(|error| NixError::Decode {
        what: "metadata arguments",
        detail: error.to_string(),
    })?;
    serde_json::to_string(&json)
        .map(|literal| literal.replace("${", "\\${"))
        .map_err(|error| NixError::Decode {
            what: "metadata arguments",
            detail: error.to_string(),
        })
}

impl Nix {
    /// Evaluate at most one batch of names and descriptions with a 512 MiB RSS limit.
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
        let argv = self.argv(&bounded_args);
        if self.verbose {
            eprintln!("pkg: nix {}", argv.join(" "));
        }
        let mut command = std::process::Command::new(&self.executable);
        command.args(argv);
        command.env("GC_INITIAL_HEAP_SIZE", "8388608");
        command.env("GC_FREE_SPACE_DIVISOR", divisor);
        command.env("MIMALLOC_PURGE_DELAY", "0");
        if self.no_color {
            command.env("NO_COLOR", "1");
        }
        let reaped =
            process::run_child_bounded(command, 512 * 1024 * 1024).map_err(NixError::Spawn)?;
        self.finish(args, &reaped)?;
        serde_json::from_slice(&reaped.output.stdout).map_err(|error| NixError::Decode {
            what,
            detail: error.to_string(),
        })
    }
}
