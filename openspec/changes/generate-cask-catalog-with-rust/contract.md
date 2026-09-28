# Implementation contract

This contract records the final review decisions. It takes precedence over
the draft examples until those examples are updated.

FINAL MANAGER CONTRACT (amends planning draft; applies to all workers):
- All nonempty formula dependencies are excluded as formula-dependency. ALL nonempty cask dependencies are excluded as cask-dependency-integration for this version. No dependency closure engine, profile expansion, wrapper sibling logic, or depends output. Native Nix lifecycle stays single-package. This avoids unprovable vendor integration and a second dependency manager.
- macosBaseline is exactly "15.7.7"; compare numeric major/minor/patch with missing components zero.
- Artifact plans uniformly use {kind,source,target}. kind app/binary/pkg/appimage/manpage/bash-completion/zsh-completion/fish-completion. source and target strings; pkg target is null. app source bundle relative path, target bundle rename ending .app. binary source may start $APPDIR/; target basename from metadata rename or source basename. AppImage source metadata name; target derived basename without .AppImage, no token condition. No guessed app CLI wrappers; only explicit binary artifacts.
- plan = {source:{url,sha256},archive:{kind:"auto"|"raw-binary"|"appimage"},artifacts:[...],minMacos:string|null}. auto content sniff supports extensionless archive URLs. pkg is an artifact inside auto container or raw xar.
- generated catalog top = {schema:"pkg-cask-catalog/2",generator:{name:"cask-catalog",version:"0.1.0"},input:{url,revision,sha256,license},targets:["aarch64-darwin","x86_64-linux"],macosBaseline:"15.7.7",entries:{TOKEN:{token,name,description,version,homepage,targets:{SYSTEM:{status,kind,reason,detail,version,homepage,plan}}}}}. name/description/version/homepage nullable strings at entry; target version/homepage are effective merged values. eligible -> kind string, reason/detail null, plan object; excluded -> kind null, reason code, detail nullable string, plan null. All targets present for all valid token identifiers. Reject invalid token IDs as whole-catalog integrity errors (not usable as map keys/path names). Unique valid token regex [a-z0-9][a-z0-9+._-]* without .. path components.
- Nix catalogIndex = SAME top provenance fields, but replaces entries with systems:{SYSTEM:{entries:{TOKEN:{token,name,description,version,homepage,status,kind,reason,detail}}}}. target's effective version/homepage used. No plan exposed to client. targets checked first. No depends field.
- Nix catalogStatus = provenance + per-system counts total/eligible/excluded/byReason. Flake only nixpkgs pinned existing rev567a49d1. Allow vendor proprietary binaries through explicit Nix import config.allowUnfree=true, no --impure client requirement.
- strict required supported_platforms. No OS guesses from variations/artifacts. Retain exact pinned input245947c0b920cbe83f4003bb7c6be737352c8314.
- Do not add production self-test command; focused cargo test suffices.
- Linux probes succeeded (1password-cli2.39.0 and KOReader v2026.07.1 --help). GUI untested.
- Full catalog output and index represent metadata eligibility, not verification. No per-token flags/allowlists. A raw .pkg eligible plan must still pass strict payload script/layout checks at build.
- Parent owns integration/verification and final PR. No user confirmation needed.
