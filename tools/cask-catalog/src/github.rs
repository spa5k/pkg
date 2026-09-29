//! GitHub public API access for source identity only (crate-internal).
//!
//! Two unauthenticated HTTPS calls resolve the current default-branch
//! head and repository license for a canonical `owner/tap`. No token,
//! no credentials, no other endpoints. Responses are parsed defensively
//! with strict shape checks; the returned commit must be an exact
//! 40-hex id or the whole operation fails.

use crate::net::SafeClient;
use serde_json::Value;

fn short(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    // Never cut inside a multi-byte character: back up to a boundary.
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// One bounded JSON GET against api.github.com.
fn api_get(client: &SafeClient, path: &str) -> Result<Value, String> {
    let url = format!("https://api.github.com{path}");
    let body = client.get(&url, Some("application/vnd.github+json"))?;
    serde_json::from_slice(&body)
        .map_err(|e| format!("api.github.com{path} returned malformed JSON: {e}"))
}

/// Pure check: the repository response names exactly `owner/repo`
/// (case-insensitive). Transfer, rename, and cross-repo redirects all
/// surface as a `full_name` mismatch and are refused.
fn expect_full_name(repo_json: &Value, owner: &str, repo: &str) -> Result<(), String> {
    let full_name = repo_json
        .get("full_name")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("repository {owner}/{repo} response has no full_name"))?;
    if !full_name.eq_ignore_ascii_case(&format!("{owner}/{repo}")) {
        return Err(format!(
            "repository {owner}/{repo} resolves to full_name {full_name:?}: transfer, \
             rename, or cross-repo redirect; refusing the import"
        ));
    }
    Ok(())
}

/// Pure extraction of a SAFE default branch name from a repository
/// response: non-empty, bounded, plain ASCII word characters plus
/// `-_. /`.
fn validated_default_branch(repo_json: &Value, owner: &str, repo: &str) -> Result<String, String> {
    let default_branch = repo_json
        .get("default_branch")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("repository {owner}/{repo} has no default_branch"))?;
    if default_branch.is_empty()
        || default_branch.len() > 244
        || default_branch
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')))
    {
        return Err(format!(
            "repository {owner}/{repo} default_branch {default_branch:?} is not a safe branch name"
        ));
    }
    Ok(default_branch.to_string())
}

/// Pure extraction of the repository license SPDX id, when reported.
fn license_from(repo_json: &Value) -> Option<String> {
    repo_json
        .get("license")
        .and_then(|l| l.get("spdx_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

/// Resolve the head commit sha of one branch (or `HEAD` for names
/// containing `/`): the response `sha` must be an exact 40-hex id.
fn head_sha(
    client: &SafeClient,
    owner: &str,
    repo: &str,
    default_branch: &str,
) -> Result<String, String> {
    let branch_ref = if default_branch.contains('/') {
        "HEAD"
    } else {
        default_branch
    };
    let branch_json = api_get(
        client,
        &format!("/repos/{owner}/{repo}/commits/{branch_ref}"),
    )
    .map_err(|e| short(&e, 400))?;
    let revision = branch_json
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("commit response for {owner}/{repo}/{default_branch} has no sha"))?
        .to_ascii_lowercase();
    if !crate::valid_revision(&revision) {
        return Err(format!(
            "commit response for {owner}/{repo}/{default_branch} is not an exact \
             40-hex commit id: {revision:?}"
        ));
    }
    Ok(revision)
}

/// Verified repository identity: the exact revision plus license.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoIdentity {
    /// Exact 40-hex commit id (explicit revision confirmed, or the
    /// default branch head when none was given).
    pub revision: String,
    /// Repository license SPDX id, when GitHub reports one.
    pub license_spdx: Option<String>,
}

/// Verify the public repository identity BEFORE any fetch.
///
/// `full_name` must equal `owner/repo` exactly (lowercased), even when
/// an explicit revision is given: transfer, rename, and cross-repo
/// redirects all surface as a `full_name` mismatch and are refused.
/// With `revision == None` the default branch head resolves; with
/// `Some(rev)` the commit endpoint must return that exact sha.
pub fn verify_repo(
    client: &SafeClient,
    owner: &str,
    repo: &str,
    revision: Option<&str>,
) -> Result<RepoIdentity, String> {
    // ONE repository GET: the same validated response provides the
    // identity check (full_name), the default branch, and the license.
    // There is no second repository request that could ignore the
    // validated full_name.
    let repo_json =
        api_get(client, &format!("/repos/{owner}/{repo}")).map_err(|e| short(&e, 400))?;
    expect_full_name(&repo_json, owner, repo)?;
    let default_branch = validated_default_branch(&repo_json, owner, repo)?;
    let license_spdx = license_from(&repo_json);
    let revision = match revision {
        Some(rev) => {
            let commit_json = api_get(client, &format!("/repos/{owner}/{repo}/commits/{rev}"))
                .map_err(|e| short(&e, 400))?;
            let sha = commit_json
                .get("sha")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("commit response for {owner}/{repo}@{rev} has no sha"))?
                .to_ascii_lowercase();
            if sha != rev.to_ascii_lowercase() || !crate::valid_revision(&sha) {
                return Err(format!(
                    "commit response for {owner}/{repo}@{rev} is not the exact requested \
                     40-hex commit id"
                ));
            }
            sha
        }
        None => head_sha(client, owner, repo, &default_branch)?,
    };
    Ok(RepoIdentity {
        revision,
        license_spdx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn full_name_check_accepts_only_the_approved_repository() {
        let ok = json!({"full_name": "owner/homebrew-tap"});
        assert!(expect_full_name(&ok, "owner", "homebrew-tap").is_ok());
        // Case difference alone is accepted (GitHub is case-insensitive).
        let cased = json!({"full_name": "Owner/HomeBrew-Tap"});
        assert!(expect_full_name(&cased, "owner", "homebrew-tap").is_ok());
        // A transferred/renamed repository surfaces as a mismatch.
        for bad in [
            json!({"full_name": "other/homebrew-tap"}),
            json!({"full_name": "owner/renamed-repo"}),
            json!({}),
            json!({"full_name": 42}),
        ] {
            let err = expect_full_name(&bad, "owner", "homebrew-tap").expect_err("refused");
            assert!(
                err.contains("refusing the import") || err.contains("no full_name"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn default_branch_and_license_extraction_is_strict() {
        let ok = json!({
            "default_branch": "main",
            "license": {"spdx_id": "MIT"},
        });
        assert_eq!(validated_default_branch(&ok, "o", "r").unwrap(), "main");
        assert_eq!(license_from(&ok).as_deref(), Some("MIT"));
        for bad in [
            json!({"default_branch": ""}),
            json!({"default_branch": "a b"}),
            json!({"default_branch": "a\tb"}),
            json!({"default_branch": "x".repeat(245)}),
            json!({}),
        ] {
            assert!(
                validated_default_branch(&bad, "o", "r").is_err(),
                "{bad} must be refused as a default branch"
            );
        }
        // Missing, null, or empty license ids map to None.
        assert_eq!(license_from(&json!({})), None);
        assert_eq!(license_from(&json!({"license": {"spdx_id": ""}})), None);
        assert_eq!(license_from(&json!({"license": null})), None);
    }

    #[test]
    fn short_bounds_error_text() {
        let long = "x".repeat(1000);
        assert_eq!(short(&long, 400).len(), 400);
        // Multi-byte text never panics and keeps whole characters.
        let unicode = "é".repeat(500);
        let cut = short(&unicode, 400);
        assert!(cut.len() <= 400);
        assert!(cut.chars().all(|c| c == 'é'));
        assert_eq!(cut.len() % 2, 0);
    }
}
