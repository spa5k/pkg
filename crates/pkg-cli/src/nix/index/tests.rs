use super::*;

/// Invalid declared ranges fail the whole decode, named by entry.
#[test]
fn invalid_macos_ranges_fail_closed() {
    let manifest = include_str!("../../../tests/fixtures/catalog-index.json");
    let mut raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
    let (system, id) = raw["systems"]
        .as_object()
        .and_then(|systems| {
            systems.keys().next().map(|system| {
                let id = systems[system]["entries"]
                    .as_object()
                    .and_then(|entries| entries.keys().next().cloned())
                    .expect("fixture entry");
                (system.clone(), id)
            })
        })
        .expect("fixture system");
    // Non-numeric bound.
    raw["systems"][&system]["entries"][&id]["minMacos"] = serde_json::json!("Sonoma");
    assert!(decode_catalog_index(&raw).is_err());
    // Empty bound.
    raw["systems"][&system]["entries"][&id]["minMacos"] = serde_json::json!("");
    assert!(decode_catalog_index(&raw).is_err());
    // Inverted range.
    raw["systems"][&system]["entries"][&id]["minMacos"] = serde_json::json!("15");
    raw["systems"][&system]["entries"][&id]["maxMacos"] = serde_json::json!("14");
    assert!(decode_catalog_index(&raw).is_err());
    // Oversized component that overflows u64: rejected, never zero.
    raw["systems"][&system]["entries"][&id]["minMacos"] = serde_json::json!("99999999999999999999");
    assert!(decode_catalog_index(&raw).is_err());
    // Signed component is rejected too.
    raw["systems"][&system]["entries"][&id]["minMacos"] = serde_json::json!("-15");
    assert!(decode_catalog_index(&raw).is_err());
    // A coherent range decodes.
    raw["systems"][&system]["entries"][&id]["minMacos"] = serde_json::json!("15");
    raw["systems"][&system]["entries"][&id]["maxMacos"] = serde_json::json!("16");
    let index = decode_catalog_index(&raw).expect("coherent range decodes");
    let entry = &index.systems[&system].entries[&id];
    assert_eq!(entry.min_macos.as_deref(), Some("15"));
    assert_eq!(entry.max_macos.as_deref(), Some("16"));
}
/// Real backend raw provenance carries `envelopeSchema` beside the
/// three hashes; the decoder accepts it (and any other growth) while
/// official inputs stay without `raw`.
#[test]
fn raw_provenance_accepts_the_backend_envelope_shape() {
    let revision = "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a";
    let raw = serde_json::json!({
        "schema": CATALOG_INDEX_SCHEMA,
        "generator": {"name": "cask-catalog", "version": "0.1.0"},
        "inputs": {"somebody/apps": {
            "url": "https://github.com/somebody/homebrew-apps",
            "revision": revision,
            "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
            "license": "Homebrew data license",
            "raw": {
                "archiveUrl": format!(
                    "https://api.github.com/repos/somebody/homebrew-apps/tarball/{revision}"),
                "archiveSha256": "2f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529252",
                "captureSha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                "envelopeSchema": "pkg-tap-raw-export/1",
            }}},
        "targets": ["x86_64-linux"],
        "macosBaseline": "15.7.7",
        "systems": {"x86_64-linux": {"entries": {
            "somebody/apps/tool": {
                "source": "somebody/apps", "token": "tool", "name": null,
                "description": null, "version": "1.0", "homepage": null,
                "status": "eligible", "kind": "binary",
                "reason": null, "detail": null}}}},
    });
    let index = decode_catalog_index(&raw).expect("decodes the backend shape");
    let input = &index.inputs["somebody/apps"];
    let raw = input.raw.as_ref().expect("raw preserved");
    assert_eq!(raw.envelope_schema.as_deref(), Some("pkg-tap-raw-export/1"));
    assert_eq!(
        raw.capture_sha256,
        "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251"
    );
    // The official catalog input carries no `raw` at all and decodes.
    let official = serde_json::json!({
        "schema": CATALOG_INDEX_SCHEMA,
        "generator": {"name": "cask-catalog", "version": "0.1.0"},
        "inputs": {"homebrew/cask": {
            "url": "github:spa5k/pkg/2403?dir=nix/casks", "revision": revision,
            "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
            "license": "Homebrew data license"}},
        "targets": ["x86_64-linux"],
        "macosBaseline": "15.7.7",
        "systems": {"x86_64-linux": {"entries": {
            "homebrew/cask/iterm2": {
                "source": "homebrew/cask", "token": "iterm2", "name": null,
                "description": null, "version": "1.0", "homepage": null,
                "status": "eligible", "kind": "binary",
                "reason": null, "detail": null}}}},
    });
    let index = decode_catalog_index(&official).expect("official raw stays absent");
    assert!(index.inputs["homebrew/cask"].raw.is_none());
}

/// Decodes the fixture: an actual generated-catalog slice (revision
/// `245947c0`, schema `pkg-cask-catalog/4`) projected to the flake
/// `catalogIndex` shape by the same rule `nix/casks/flake.nix` uses.
/// Values are real generated data, not invented examples. The official
/// catalog is one source among possibly many: its identity is
/// `homebrew/cask` and every entry key repeats source and token.
#[test]
fn decodes_the_actual_catalog_index_slice() {
    let manifest = include_str!("../../../tests/fixtures/catalog-index.json");
    let raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
    let index = decode_catalog_index(&raw).expect("decodes actual slice");
    assert_eq!(index.generator.name, "cask-catalog");
    assert_eq!(index.generator.version, "0.1.0");
    let official = &index.inputs[OFFICIAL_CASK_SOURCE];
    assert_eq!(
        official.revision,
        "245947c0b920cbe83f4003bb7c6be737352c8314"
    );
    assert_eq!(
        official.sha256,
        "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251"
    );
    assert_eq!(official.license, "BSD-2-Clause (Homebrew Cask data)");
    assert_eq!(index.targets, ["aarch64-darwin", "x86_64-linux"]);
    assert_eq!(index.macos_baseline, "15.7.7");

    let macos = &index.systems["aarch64-darwin"].entries;
    let iterm2 = &macos["homebrew/cask/iterm2"];
    assert_eq!(iterm2.source, OFFICIAL_CASK_SOURCE);
    assert_eq!(iterm2.token, "iterm2");
    assert_eq!(iterm2.status, EntryStatus::Eligible);
    assert_eq!(iterm2.kind.as_deref(), Some("app"));
    assert_eq!(iterm2.version.as_deref(), Some("3.6.11"));
    assert_eq!(iterm2.name.as_deref(), Some("iTerm2"));
    // Special token characters are real identifiers in the catalog.
    assert_eq!(
        macos["homebrew/cask/1password@7"].status,
        EntryStatus::Eligible
    );
    assert_eq!(
        macos["homebrew/cask/4k-video-downloader+"].status,
        EntryStatus::Eligible
    );
    // One token can be eligible on one target and excluded on another,
    // with a target-effective version that differs from the entry.
    let cursor_linux = &index.systems["x86_64-linux"].entries["homebrew/cask/cursor"];
    assert_eq!(cursor_linux.status, EntryStatus::Eligible);
    assert_eq!(cursor_linux.kind.as_deref(), Some("appimage"));
    let raycast_linux = &index.systems["x86_64-linux"].entries["homebrew/cask/raycast"];
    assert_eq!(raycast_linux.status, EntryStatus::Excluded);
    assert_eq!(
        raycast_linux.reason.as_deref(),
        Some("unsupported-platform")
    );
    // The Linux variation sets `version` to null; Nix `or` does not
    // coalesce a present null to the base value, so the real index
    // carries null here — the fixture must match that exactly.
    assert_eq!(raycast_linux.version, None);
    // Excluded records carry their generated reason; detail stays
    // string-or-null.
    let zoom = &macos["homebrew/cask/zoom"];
    assert_eq!(zoom.status, EntryStatus::Excluded);
    assert_eq!(zoom.reason.as_deref(), Some("installer-script"));
    assert_eq!(zoom.kind, None);
    // The Linux lane that was probed for real is eligible here.
    let koreader = &index.systems["x86_64-linux"].entries["homebrew/cask/koreader"];
    assert_eq!(koreader.status, EntryStatus::Eligible);
    assert_eq!(koreader.version.as_deref(), Some("2026.07.1"));
}

/// Nullable metadata is part of the protocol: a synthetic minimal
/// record proves null name/description/version/homepage decode without
/// inventing values (the real slice carries non-null strings). The same
/// envelope proves two sources may carry the same bare token: they
/// coexist under different full identities.
#[test]
fn nullable_metadata_stays_null_and_tokens_coexist_per_source() {
    let input = |url: &str| {
        serde_json::json!({
            "url": url,
            "revision": "245947c0b920cbe83f4003bb7c6be737352c8314",
            "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
            "license": "Homebrew data license",
            "extra-unknown-field": {"any": ["shape"]}
        })
    };
    let record = |source: &str, status: &str| {
        let eligible = status == "eligible";
        serde_json::json!({
                "source": source, "token": "bare", "name": null,
                "description": null, "version": null, "homepage": null,
                "status": status,
                "kind": if eligible { serde_json::json!("binary") } else { serde_json::Value::Null },
                "reason": if eligible { serde_json::Value::Null } else { serde_json::json!("unsupported-platform") },
                "detail": null})
    };
    let raw: serde_json::Value = serde_json::json!({
        "schema": CATALOG_INDEX_SCHEMA,
        "generator": {"name": "cask-catalog", "version": "0.1.0"},
        "inputs": {
            "homebrew/cask": input("https://example.com/official.json"),
            "somebody/apps": input("https://github.com/somebody/homebrew-apps")
        },
        "targets": ["x86_64-linux"],
        "macosBaseline": "15.7.7",
        "systems": {"x86_64-linux": {"entries": {
            "homebrew/cask/bare": record(OFFICIAL_CASK_SOURCE, "eligible"),
            "somebody/apps/bare": record("somebody/apps", "excluded")}}}
    });
    let index = decode_catalog_index(&raw).expect("decodes nullable records");
    let entries = &index.systems["x86_64-linux"].entries;
    let bare = &entries["homebrew/cask/bare"];
    assert_eq!(bare.name, None);
    assert_eq!(bare.description, None);
    assert_eq!(bare.version, None);
    assert_eq!(bare.homepage, None);
    assert_eq!(bare.status, EntryStatus::Eligible);
    // The same bare token from a second source coexists; identity is
    // the full id, never the token alone.
    let tap_bare = &entries["somebody/apps/bare"];
    assert_eq!(tap_bare.status, EntryStatus::Excluded);
    assert_eq!(tap_bare.source, "somebody/apps");
    assert_eq!(index.inputs.len(), 2);
}

/// The envelope serializes back to the exact decode shape — published
/// captures are written by the decoder's own types, and the full
/// generated catalog layout (no `systems` sections) is refused here.
#[test]
fn envelope_round_trips_and_full_catalog_layout_is_refused() {
    let manifest = include_str!("../../../tests/fixtures/catalog-index.json");
    let raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
    let index = decode_catalog_index(&raw).expect("decodes");
    let encoded = serde_json::to_value(&index).expect("encodes");
    // The written capture carries the schema identity and the exact
    // `macosBaseline` field name, so saved captures decode again.
    assert_eq!(encoded["schema"], CATALOG_INDEX_SCHEMA);
    assert_eq!(encoded["macosBaseline"], "15.7.7");
    assert_eq!(decode_catalog_index(&encoded).expect("round trips"), index);

    // A layout without per-system `systems` sections is backend data;
    // this client refuses it instead of projecting it.
    let mut full = raw;
    let systems = full["systems"].clone();
    full.as_object_mut()
        .expect("object")
        .insert(String::from("entries"), systems);
    full.as_object_mut().expect("object").remove("systems");
    assert!(decode_catalog_index(&full).is_err());
}

/// The schema gate names the schema found and the schema supported, so a
/// future catalog (or an old envelope) is a clear error, never a misread.
#[test]
fn catalog_index_schema_gate_names_both_schemas() {
    let manifest = include_str!("../../../tests/fixtures/catalog-index.json");
    let mut raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
    raw["schema"] = serde_json::json!("pkg-cask-catalog/5");
    let error = decode_catalog_index(&raw).expect_err("future schema is refused");
    let text = error.to_string();
    assert!(text.contains("pkg-cask-catalog/5"), "{text}");
    // The previous schema 3 (before the macOS range gate) is refused
    // by the same gate: an old catalog cannot bypass the new bounds.
    raw["schema"] = serde_json::json!("pkg-cask-catalog/3");
    assert!(decode_catalog_index(&raw).is_err());
    assert!(text.contains(CATALOG_INDEX_SCHEMA), "{text}");
    // The removed schema 2 is refused by the same gate.
    raw["schema"] = serde_json::json!("pkg-cask-catalog/2");
    assert!(decode_catalog_index(&raw).is_err());
    raw.as_object_mut().unwrap().remove("schema");
    let error = decode_catalog_index(&raw).expect_err("missing schema is refused");
    assert!(error.to_string().contains("no schema"));
}

/// A record that breaks the identity rules, the null rules, or the
/// provenance rules fails the whole decode with the entry named; a
/// missing target section or an undeclared system section fails the
/// envelope as inconsistent.
#[test]
fn broken_records_fail_the_whole_decode_named() {
    let manifest = include_str!("../../../tests/fixtures/catalog-index.json");
    let mut raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
    let pristine = raw.clone();
    // An eligible entry without a kind breaks the null rules.
    raw["systems"]["x86_64-linux"]["entries"]["homebrew/cask/koreader"]["kind"] =
        serde_json::Value::Null;
    let error = decode_catalog_index(&raw).expect_err("broken record fails the decode");
    let text = error.to_string();
    assert!(text.contains("koreader"), "{text}");
    assert!(text.contains("kind"), "{text}");

    // A record token that does not match its key fails the decode.
    let mut mismatched = raw.clone();
    mismatched["systems"]["x86_64-linux"]["entries"]["homebrew/cask/koreader"]["token"] =
        serde_json::json!("other-token");
    let error = decode_catalog_index(&mismatched).expect_err("token mismatch fails");
    assert!(error.to_string().contains("other-token"));

    // A record source that does not match its key fails the decode.
    let mut foreign = raw.clone();
    foreign["systems"]["x86_64-linux"]["entries"]["homebrew/cask/koreader"]["source"] =
        serde_json::json!("somebody/apps");
    let error = decode_catalog_index(&foreign).expect_err("source mismatch fails");
    assert!(error.to_string().contains("source"));

    // A record whose source carries no provenance fails the decode.
    let mut unprovenanced = pristine;
    unprovenanced["systems"]["x86_64-linux"]["entries"]
        .as_object_mut()
        .unwrap()
        .insert(
            String::from("somebody/apps/koreader"),
            serde_json::json!({
                    "source": "somebody/apps", "token": "koreader",
                    "name": null, "description": null, "version": null,
                    "homepage": null, "status": "eligible", "kind": "binary",
                    "reason": null, "detail": null}),
        );
    let error = decode_catalog_index(&unprovenanced).expect_err("unprovenanced source fails");
    assert!(error.to_string().contains("somebody/apps"));

    // A key that is not a full owner/tap/token identity cannot be a
    // map key: a bare token fails, and so does an invalid token.
    let mut keyed = raw.clone();
    let record = keyed["systems"]["x86_64-linux"]["entries"]["homebrew/cask/koreader"].clone();
    keyed["systems"]["x86_64-linux"]["entries"]
        .as_object_mut()
        .unwrap()
        .insert(String::from("koreader"), record.clone());
    let error = decode_catalog_index(&keyed).expect_err("bare key fails");
    assert!(error.to_string().contains("koreader"));
    let mut upper = raw.clone();
    upper["systems"]["x86_64-linux"]["entries"]
        .as_object_mut()
        .unwrap()
        .insert(String::from("homebrew/cask/Iterm2"), record);
    let error = decode_catalog_index(&upper).expect_err("invalid token key fails");
    assert!(error.to_string().contains("Iterm2"));

    // A declared target without a system section is an inconsistent
    // envelope and fails as a whole.
    let mut broken = raw.clone();
    broken["systems"]
        .as_object_mut()
        .unwrap()
        .remove("aarch64-darwin");
    let error = decode_catalog_index(&broken).expect_err("missing target section");
    assert!(error.to_string().contains("aarch64-darwin"));
    // An undeclared system section fails too.
    let mut extra = raw.clone();
    extra["systems"]["x86_64-darwin"] = extra["systems"]["x86_64-linux"].clone();
    let error = decode_catalog_index(&extra).expect_err("undeclared system section");
    assert!(error.to_string().contains("x86_64-darwin"));

    // An inputs provenance object without required data fails closed.
    let mut pinless = raw.clone();
    pinless["inputs"][OFFICIAL_CASK_SOURCE]["revision"] = serde_json::json!("");
    let error = decode_catalog_index(&pinless).expect_err("empty provenance fails");
    assert!(error.to_string().contains("revision"), "{}", error);
}

/// Full entry identities split and reassemble exactly, and the source
/// slug rule accepts the official source and real tap slugs while
/// refusing traversal and malformed input.
#[test]
fn full_entry_identities_split_and_reassemble() {
    assert_eq!(
        split_entry_id("somebody/apps/iterm2"),
        Some(("somebody/apps", "iterm2"))
    );
    assert_eq!(
        split_entry_id(OFFICIAL_CASK_SOURCE),
        None,
        "an entry id needs a token segment"
    );
    assert_eq!(split_entry_id("somebody/apps"), None);
    assert_eq!(split_entry_id("somebody/apps/a/b"), None);
    assert_eq!(
        split_entry_id("somebody/apps/1password-cli@beta")
            .unwrap()
            .1,
        "1password-cli@beta"
    );
    assert_eq!(
        full_entry_id(OFFICIAL_CASK_SOURCE, "iterm2"),
        "homebrew/cask/iterm2"
    );

    assert!(valid_catalog_source(OFFICIAL_CASK_SOURCE));
    assert!(valid_catalog_source("somebody/apps"));
    assert!(valid_catalog_source("a-b.c/d_e"));
    for refused in [
        "",
        "somebody",
        "somebody/apps/extra",
        "/apps",
        "somebody/",
        "../apps",
        "somebody/..",
        "some body/apps",
        "somebody/ap ps",
        "somebody/apps?dir=x",
    ] {
        assert!(!valid_catalog_source(refused), "{refused} must be refused");
    }
}

/// The token rule matches the generator's identifier rule, including
/// the real `@` versioned tokens and `+` tokens of the snapshot.
#[test]
fn catalog_tokens_follow_the_generated_identifier_rule() {
    for token in [
        "a",
        "iterm2",
        "1password-cli",
        "1password-cli@beta",
        "1password@nightly",
        "xournal++",
        "4k-video-downloader+",
        "telegram+bot",
        "v2.0",
    ] {
        assert!(valid_catalog_token(token), "{token}");
    }
    for token in [
        "",
        "Iterm2",
        "-lead",
        ".lead",
        "a..b",
        "a/b",
        "to ken",
        "1password cli",
    ] {
        assert!(!valid_catalog_token(token), "{token}");
    }
}
