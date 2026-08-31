//! User-intent selectors: what the user asked for (D-13), as distinct from the
//! exact realized artifact.
//!
//! Selectors live in the manifest (`plans/01` §10.1 / `plans/05` §5.1). This
//! module holds **only** the PR-2 intent fields — no timestamps, provenance, or
//! schema containers. `pname@version` is display metadata and never appears
//! here as an identity.

use std::fmt;
use std::str::FromStr;

use crate::identity::{OutputName, RealizationIdentity};
use crate::version::VersionPreference;

/// Error returned when a selector value cannot be parsed or validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorError {
    /// A selector id failed validation.
    InvalidSelectorId {
        /// The rejected input.
        input: String,
    },
    /// A user selector input failed validation.
    InvalidSelectorInput {
        /// The rejected input.
        input: String,
    },
    /// An attribute path failed validation.
    InvalidAttributePath {
        /// The rejected input.
        input: String,
    },
    /// An explicit output selection was empty.
    EmptyOutputSelection,
    /// An explicit output selection listed a duplicate output name.
    DuplicateOutputName,
    /// A selector was pinned while its attribute path was still unresolved.
    UnresolvedSelector,
    /// An attribute-path change was attempted on a selector that is already
    /// pinned; the attribute is frozen once pinned.
    AttributeChangeWhilePinned,
}

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectorError::InvalidSelectorId { input } => write!(
                f,
                "invalid selector id {input:?}: must be `sel_` followed by a nonempty [A-Za-z0-9_-] suffix"
            ),
            SelectorError::InvalidSelectorInput { input } => write!(
                f,
                "invalid selector input {input:?}: must be a nonempty [A-Za-z0-9._-] string"
            ),
            SelectorError::InvalidAttributePath { input } => write!(
                f,
                "invalid attribute path {input:?}: dot-separated nonempty [A-Za-z0-9_-] segments"
            ),
            SelectorError::EmptyOutputSelection => {
                f.write_str("explicit output selection must be nonempty")
            }
            SelectorError::DuplicateOutputName => {
                f.write_str("explicit output selection must not contain duplicates")
            }
            SelectorError::UnresolvedSelector => {
                f.write_str("cannot pin a selector whose attribute path is unresolved")
            }
            SelectorError::AttributeChangeWhilePinned => {
                f.write_str("cannot change the attribute path of a pinned selector")
            }
        }
    }
}

impl std::error::Error for SelectorError {}

/// A stable, opaque manifest id for a selector entry (`plans/01` §10.1 `id`).
///
/// Requires the `sel_` prefix followed by a nonempty `[A-Za-z0-9_-]` suffix.
/// ULID length is **not** enforced (plan examples are schematic and
/// inconsistent) — only the prefix/character grammar.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SelectorId(String);

impl SelectorId {
    /// Validates and constructs a selector id.
    pub fn new(value: &str) -> Result<Self, SelectorError> {
        if is_selector_id(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SelectorError::InvalidSelectorId {
                input: value.to_owned(),
            })
        }
    }

    /// Returns the id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SelectorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SelectorId {
    type Err = SelectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Returns `true` if `s` is a valid [`SelectorId`]: `sel_` + a nonempty
/// `[A-Za-z0-9_-]` suffix.
fn is_selector_id(s: &str) -> bool {
    let rest = match s.strip_prefix("sel_") {
        Some(r) => r,
        None => return false,
    };
    !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A user-typed selector input (`plans/01` §10.1 `selector`; `plans/01` §11.1
/// allowlist grammar).
///
/// Validated as a nonempty `[A-Za-z0-9._-]+` string. This is an intent string,
/// **not** a Nix attribute-path expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SelectorInput(String);

impl SelectorInput {
    /// Validates and constructs a selector input.
    pub fn new(value: &str) -> Result<Self, SelectorError> {
        if is_selector_input(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SelectorError::InvalidSelectorInput {
                input: value.to_owned(),
            })
        }
    }

    /// Returns the selector input string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SelectorInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SelectorInput {
    type Err = SelectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Returns `true` if `s` is a valid [`SelectorInput`] (nonempty
/// `[A-Za-z0-9._-]+`).
fn is_selector_input(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A conservative v1 Nixpkgs attribute-path fragment (`plans/01` §10.1
/// `attribute`).
///
/// Dot-separated, with no empty segments; each segment contains only ASCII
/// alphanumeric characters, `_`, or `-`. The raw-Nix characters `#`, `?`, `/`,
/// whitespace, and control bytes are rejected. This is intentionally **not**
/// the full Nix attribute-path grammar — only a safe v1 fragment.
///
/// **PR-3 NixAdapter contract:** consumers must treat the segments from
/// [`AttributePath::segments`] as opaque **data** and pass them to Nix through
/// a structured interface or with proper quoting/encoding. They must **never**
/// interpolate [`AttributePath::as_str`] (or any segment) into a raw Nix
/// expression string. This allowlist is defense in depth, not a license to
/// build Nix source by concatenation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttributePath(String);

impl AttributePath {
    /// Validates and constructs an attribute path.
    pub fn new(value: &str) -> Result<Self, SelectorError> {
        if is_attribute_path(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SelectorError::InvalidAttributePath {
                input: value.to_owned(),
            })
        }
    }

    /// Returns the attribute-path string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Iterates the dot-separated segments.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }
}

impl fmt::Display for AttributePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AttributePath {
    type Err = SelectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Returns `true` if `s` is a valid [`AttributePath`].
fn is_attribute_path(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Reject any byte that is a separator we explicitly forbid, or whitespace,
    // or control. Allow ASCII alnum and `_`/`-`; `.` is the segment separator.
    for b in s.bytes() {
        let allowed = b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.');
        if !allowed {
            return false;
        }
    }
    // No empty segments.
    s.split('.').all(|seg| !seg.is_empty())
}

/// Which outputs of a derivation to install (`plans/01` §10.1 `outputs`).
///
/// The meta default (no explicit list) means `null` → use
/// `meta.outputsToInstall` (`plans/04` §12.1). An explicit selection is a
/// **nonempty, duplicate-free** list of [`OutputName`].
///
/// The field is private, so an invalid explicit state (empty list or a list
/// with duplicates) is **impossible to construct from the public API**: use
/// [`OutputSelection::default_selection`] for the meta default, or the
/// validating [`OutputSelection::explicit`] constructor for an explicit list.
/// The intended wire form maps the default to `null` and an explicit list to a
/// JSON array.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct OutputSelection(Option<Vec<OutputName>>);

impl OutputSelection {
    /// The meta default: defer to `meta.outputsToInstall`.
    #[must_use]
    pub const fn default_selection() -> Self {
        OutputSelection(None)
    }

    /// Constructs an explicit selection, rejecting empty lists and duplicates.
    pub fn explicit(outputs: Vec<OutputName>) -> Result<Self, SelectorError> {
        if outputs.is_empty() {
            return Err(SelectorError::EmptyOutputSelection);
        }
        let mut seen = std::collections::HashSet::new();
        for name in &outputs {
            if !seen.insert(name) {
                return Err(SelectorError::DuplicateOutputName);
            }
        }
        Ok(OutputSelection(Some(outputs)))
    }

    /// Returns `true` if this is the meta default.
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.0.is_none()
    }

    /// Returns the explicit output list, if any. `None` means the meta
    /// default; `Some(slice)` means an explicit, validated, nonempty,
    /// duplicate-free list.
    #[must_use]
    pub fn explicit_outputs(&self) -> Option<&[OutputName]> {
        self.0.as_deref()
    }
}

/// Whether a selector is pinned, and if so to which realized identity
/// (`plans/01` §10.1 `pinned`/`pinnedTo`; `plans/05` §5.1).
///
/// Never modeled as a `bool` plus an `Option`; the pinned identity (when any)
/// is carried by the [`RealizationIdentity`] here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum PinState {
    /// Not pinned; upgrades may move this selector.
    #[default]
    Unpinned,
    /// Pinned to an exact realized identity.
    Pinned(RealizationIdentity),
}

impl PinState {
    /// Returns `true` if this selector is pinned.
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        matches!(self, PinState::Pinned(_))
    }

    /// Returns the pinned identity, if any.
    #[must_use]
    pub const fn pinned_identity(&self) -> Option<&RealizationIdentity> {
        match self {
            PinState::Unpinned => None,
            PinState::Pinned(id) => Some(id),
        }
    }
}

/// A full user-intent selector entry — the PR-2 intent vocabulary
/// (`plans/01` §10.1 / `plans/04` §4.1 / `plans/05` §5.1).
///
/// Holds only intent fields: no timestamps, provenance, or schema containers.
/// Fields are private and accessed via methods.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageSelector {
    id: SelectorId,
    selector: SelectorInput,
    attribute: Option<AttributePath>,
    version_preference: VersionPreference,
    outputs: OutputSelection,
    source_revision: crate::channel::SourceRevision,
    pin_state: PinState,
}

impl PackageSelector {
    /// Constructs a new selector intent.
    ///
    /// `source_revision` is **required**: the caller supplies the exact revision
    /// this selector resolves against — typically
    /// [`SourceRevision::CurrentChannel`](crate::channel::SourceRevision). The
    /// attribute is [`None`] until resolved via
    /// [`PackageSelector::with_attribute`].
    #[must_use]
    pub const fn new(
        id: SelectorId,
        selector: SelectorInput,
        version_preference: VersionPreference,
        outputs: OutputSelection,
        source_revision: crate::channel::SourceRevision,
    ) -> Self {
        Self {
            id,
            selector,
            attribute: None,
            version_preference,
            outputs,
            source_revision,
            pin_state: PinState::Unpinned,
        }
    }

    /// Returns the stable selector id.
    #[must_use]
    pub const fn id(&self) -> &SelectorId {
        &self.id
    }

    /// Returns the user-typed selector input.
    #[must_use]
    pub const fn selector(&self) -> &SelectorInput {
        &self.selector
    }

    /// Returns the resolved attribute path, if any.
    #[must_use]
    pub const fn attribute(&self) -> Option<&AttributePath> {
        self.attribute.as_ref()
    }

    /// Returns the version preference.
    #[must_use]
    pub const fn version_preference(&self) -> &VersionPreference {
        &self.version_preference
    }

    /// Returns the output selection.
    #[must_use]
    pub const fn outputs(&self) -> &OutputSelection {
        &self.outputs
    }

    /// Returns the source revision.
    #[must_use]
    pub const fn source_revision(&self) -> &crate::channel::SourceRevision {
        &self.source_revision
    }

    /// Returns the pin state.
    #[must_use]
    pub const fn pin_state(&self) -> &PinState {
        &self.pin_state
    }

    /// Sets (or replaces) the resolved attribute path while **unpinned**.
    ///
    /// Once a selector is pinned ([`PackageSelector::pinned_to`]) its attribute
    /// path is frozen: changing it would produce misleading pinned metadata
    /// (the pin would claim to identify an attribute it no longer names). Call
    /// [`PackageSelector::unpinned`] first to make changes.
    ///
    /// While unpinned this sets or replaces the attribute freely.
    ///
    /// # Errors
    ///
    /// Returns [`SelectorError::AttributeChangeWhilePinned`] if the selector is
    /// pinned.
    pub fn with_attribute(mut self, attribute: AttributePath) -> Result<Self, SelectorError> {
        if self.pin_state.is_pinned() {
            return Err(SelectorError::AttributeChangeWhilePinned);
        }
        self.attribute = Some(attribute);
        Ok(self)
    }

    /// Pins this selector to an exact realized identity, returning the updated
    /// selector.
    ///
    /// Returns [`SelectorError::UnresolvedSelector`] if the attribute path has
    /// not been resolved yet — a selector cannot be pinned before it is
    /// resolved to a concrete attribute (`plans/05` §5.1).
    pub fn pinned_to(mut self, identity: RealizationIdentity) -> Result<Self, SelectorError> {
        if self.attribute.is_none() {
            return Err(SelectorError::UnresolvedSelector);
        }
        self.pin_state = PinState::Pinned(identity);
        Ok(self)
    }

    /// Returns this selector unpinned.
    #[must_use]
    pub fn unpinned(mut self) -> Self {
        self.pin_state = PinState::Unpinned;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_id_valid_and_invalid() {
        let ok = ["sel_018f", "sel_abc-1_2", "sel_X", "sel_01HZXKD7V3K1Y9"];
        for s in ok {
            let id = SelectorId::new(s).unwrap();
            assert_eq!(id.as_str(), s);
            assert_eq!(id.to_string(), s);
        }
        let bad = [
            "", "sel_", "018f", "sel_bad!", // invalid char
            "sel_a b",  // space
            "sel_a.b",  // dot not allowed in id suffix
            "sel_中",   // non-ascii
            "_sel_x",   // wrong prefix position
        ];
        for s in bad {
            assert!(SelectorId::new(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn selector_input_valid_and_invalid() {
        let ok = [
            "ripgrep",
            "python3.11",
            "xorg.xf86-input-libinput",
            "a-b_c.d",
        ];
        for s in ok {
            let si = SelectorInput::new(s).unwrap();
            assert_eq!(si.as_str(), s);
        }
        let bad = ["", "bad name", "a#b", "a/b", "café", "a?b"];
        for s in bad {
            assert!(SelectorInput::new(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn attribute_path_valid_and_invalid() {
        let ok = [
            "ripgrep",
            "python311",
            "xorg.xf86_input-libinput",
            "a.b.c",
            "haskellPackages.pandoc",
        ];
        for s in ok {
            let ap = AttributePath::new(s).unwrap();
            assert_eq!(ap.as_str(), s);
            assert_eq!(ap.segments().count(), s.split('.').count());
        }
        let bad = [
            "",
            ".foo",     // empty first segment
            "foo.",     // empty last segment
            "foo..bar", // empty middle segment
            "foo#bar",
            "foo?bar",
            "foo/bar",
            "foo bar",
            "foo\tbar",
            "foo.bar/baz",
            "café",
        ];
        for s in bad {
            assert!(AttributePath::new(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn output_selection_default_and_explicit() {
        // Default selections.
        assert!(OutputSelection::default_selection().is_default());
        assert!(OutputSelection::default().is_default());
        assert!(OutputSelection::default().explicit_outputs().is_none());

        // An explicit selection carries its validated, nonempty, duplicate-free
        // list.
        let outputs = vec![
            OutputName::new("out").unwrap(),
            OutputName::new("man").unwrap(),
        ];
        let e = OutputSelection::explicit(outputs.clone()).unwrap();
        assert!(!e.is_default());
        assert_eq!(e.explicit_outputs(), Some(outputs.as_slice()));

        // Empty rejected.
        assert_eq!(
            OutputSelection::explicit(vec![]).unwrap_err(),
            SelectorError::EmptyOutputSelection
        );
        // Duplicate rejected.
        assert_eq!(
            OutputSelection::explicit(vec![
                OutputName::new("out").unwrap(),
                OutputName::new("out").unwrap(),
            ])
            .unwrap_err(),
            SelectorError::DuplicateOutputName
        );

        // Invalid explicit state is impossible from the public API: the field
        // is private and there is no public variant holding a raw list.
    }

    #[test]
    fn package_selector_construction_and_accessors() {
        let sel = PackageSelector::new(
            SelectorId::new("sel_018f").unwrap(),
            SelectorInput::new("ripgrep").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            crate::channel::SourceRevision::CurrentChannel,
        );
        assert_eq!(sel.id().as_str(), "sel_018f");
        assert_eq!(sel.selector().as_str(), "ripgrep");
        assert!(sel.attribute().is_none());
        assert!(sel.outputs().is_default());
        assert!(!sel.pin_state().is_pinned());

        // Resolve attribute.
        let id = RealizationIdentity::new(
            crate::identity::StorePath::new(
                "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-ripgrep-14.1.0",
            )
            .unwrap(),
        );
        // Pinning before resolving the attribute fails (P1).
        assert_eq!(
            sel.clone().pinned_to(id.clone()),
            Err(SelectorError::UnresolvedSelector)
        );
        let sel = sel
            .with_attribute(AttributePath::new("ripgrep").unwrap())
            .unwrap();
        assert_eq!(sel.attribute().unwrap().as_str(), "ripgrep");

        // Pin.
        let sel = sel.pinned_to(id.clone()).unwrap();
        assert!(sel.pin_state().is_pinned());
        assert_eq!(sel.pin_state().pinned_identity(), Some(&id));

        // Unpin.
        let sel = sel.unpinned();
        assert!(!sel.pin_state().is_pinned());
    }

    #[test]
    fn with_attribute_lifecycle_and_pin_freeze() {
        let mk = || {
            PackageSelector::new(
                SelectorId::new("sel_018f").unwrap(),
                SelectorInput::new("ripgrep").unwrap(),
                VersionPreference::Any,
                OutputSelection::default_selection(),
                crate::channel::SourceRevision::CurrentChannel,
            )
        };
        let id = RealizationIdentity::new(
            crate::identity::StorePath::new(
                "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-ripgrep-14.1.0",
            )
            .unwrap(),
        );

        // unresolved -> with_attribute (resolve) succeeds.
        let sel = mk()
            .with_attribute(AttributePath::new("ripgrep").unwrap())
            .unwrap();
        assert_eq!(sel.attribute().unwrap().as_str(), "ripgrep");

        // resolved -> pin succeeds.
        let pinned = sel.clone().pinned_to(id.clone()).unwrap();
        assert!(pinned.pin_state().is_pinned());

        // pinned -> with_attribute fails, and the attribute is left unchanged.
        assert_eq!(
            pinned
                .clone()
                .with_attribute(AttributePath::new("other").unwrap()),
            Err(SelectorError::AttributeChangeWhilePinned)
        );
        assert_eq!(pinned.attribute().unwrap().as_str(), "ripgrep");

        // unpin -> with_attribute succeeds again.
        let reopened = pinned
            .unpinned()
            .with_attribute(AttributePath::new("other").unwrap())
            .unwrap();
        assert_eq!(reopened.attribute().unwrap().as_str(), "other");
        assert!(!reopened.pin_state().is_pinned());
    }
}
