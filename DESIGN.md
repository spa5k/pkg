---
name: pkg
description: A package transit control room — a lifecycle transit map fused with a serious terminal workbench.
colors:
  ink: "#0f131a"
  ink-raised: "#151b25"
  ink-card: "#1b2230"
  ink-border: "#232c3b"
  rule: "#2a3344"
  rule-strong: "#364154"
  paper: "#ece5d6"
  paper-secondary: "#c5beae"
  paper-tertiary: "#8f8a7c"
  teal: "#54a99c"
  teal-deep: "#3c8478"
  amber: "#d99a4e"
  amber-deep: "#b07a35"
  rust: "#cf6f50"
  rust-deep: "#5a3b34"
  green: "#82b07a"
  scrim: "rgba(0,0,0,.35)"
  scrim-soft: "rgba(0,0,0,.18)"
typography:
  headline:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif'
    fontSize: "1.32rem"
    fontWeight: 600
    lineHeight: 1.25
    letterSpacing: "-0.01em"
  title:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif'
    fontSize: "1.12rem"
    fontWeight: 600
    lineHeight: 1.25
    letterSpacing: "-0.01em"
  body:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif'
    fontSize: "1rem"
    fontWeight: 400
    lineHeight: 1.55
  lead:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif'
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.55
  label:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif'
    fontSize: "0.74rem"
    fontWeight: 400
    lineHeight: 1.4
    letterSpacing: "0.06em"
  mono-data:
    fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", "DejaVu Sans Mono", monospace'
    fontSize: "0.82rem"
    fontWeight: 400
    lineHeight: 1.5
rounded:
  xs: "3px"
  sm: "4px"
  md: "6px"
  lg: "8px"
  pill: "999px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "20px"
components:
  terminal:
    backgroundColor: "{colors.ink-card}"
    textColor: "{colors.paper}"
    typography: "{typography.mono-data}"
    rounded: "{rounded.lg}"
    padding: "12px 14px"
  panel:
    backgroundColor: "{colors.ink-raised}"
    rounded: "{rounded.lg}"
    padding: "14px"
  station:
    backgroundColor: "{colors.ink-raised}"
    rounded: "{rounded.lg}"
    padding: "18px 20px"
  terminal-button-go:
    backgroundColor: "{colors.ink-raised}"
    textColor: "{colors.teal}"
    typography: "{typography.mono-data}"
    rounded: "{rounded.md}"
    padding: "6px 12px"
  terminal-button-cancel:
    backgroundColor: "{colors.ink-raised}"
    textColor: "{colors.rust}"
    typography: "{typography.mono-data}"
    rounded: "{rounded.md}"
    padding: "6px 12px"
  pill:
    backgroundColor: "{colors.ink-raised}"
    textColor: "{colors.paper-secondary}"
    rounded: "{rounded.pill}"
    padding: "3px 8px"
  chip:
    backgroundColor: "{colors.ink-card}"
    textColor: "{colors.paper-secondary}"
    typography: "{typography.mono-data}"
    rounded: "{rounded.pill}"
    padding: "2px 8px"
  segmented-control:
    backgroundColor: "{colors.ink-raised}"
    rounded: "{rounded.lg}"
---

# Design System: pkg

## Overview

**Creative North Star: "The Package Transit Control Room"**

The interface is a control room, not a dashboard. Two things share the canvas: a lifecycle transit map of the package's journey from pinned input to garbage collection, and a serious terminal workbench where the user types familiar commands and reads honest, illustrative output. The canvas is deep ink; text is warm paper; and only two restrained route colors carry meaning — teal for pkg's own policy and state, amber for the hidden Nix engine the user never invokes directly. The mood is calm, factual, and low-glare: legible without high contrast, dense without clutter, serious without performing seriousness.

Depth is tonal, not shadowed. Surfaces step down the ink ramp (canvas → raised panel → card) and the only shadow is a low, diffuse scrim reserved for surfaces that genuinely float — the terminal and the bottom switcher. Motion is sparse and always decorative: a short fade when a view enters, a single signal crawling the transit line, and quick 120ms color transitions on controls. Every motion path has a reduced-motion counterpart that collapses to stillness. Keyboard focus is a first-class surface, not an afterthought: a 2px paper outline appears only for keyboard users via `:focus-visible`, and the safe action in any confirmation takes initial focus so a blind Enter never triggers a build or an upgrade.

**Three-role color semantics are the spine of the system.** Every concept separates into *what you want* (neutral paper), *what pkg decides* (teal), and *what the hidden engine does* (amber). The same three roles recur in the legend bar, the route lines on the map, the station signal nodes, the command chips, the terminal prompt colors, and the role rows under each station.

> **Durable system vs. this prototype's content.** The palette, the sans/mono type pairing, the tonal depth model, the three-role semantics, the component vocabulary (terminal, panel, station, pill, chip, segmented control, role row, switcher), the keyboard-visible focus, and the reduced-motion reflexes are the durable system and apply to any pkg surface. By contrast, the three structural variants in the source prototype — *Guided Day*, *Command Desk*, *Lifecycle Map* — are **this artifact's surface concepts**, not required templates: a real screen may use one, none, or a different composition, as long as it inherits the system above. Likewise the specific commands, generations, versions, sizes, and timings shown are illustrative product content, not visual rules.

**Key Characteristics:**
- Deep ink canvas with warm paper text; restrained, semantic teal and amber route colors.
- System sans for prose and chrome, monospace for every command, output, identifier, and key.
- Flat, tonal depth via the ink ramp; a single low shadow reserved for floating surfaces.
- Three-role color semantics: user intent / pkg policy / hidden engine.
- Keyboard-first: visible `:focus-visible` outline, safe-action default focus, arrow-key view switching.
- Reduced-motion honored everywhere; calm, factual voice; illustrative data always labeled.

## Colors

A small, warm-neutral palette built on two ramps — a deep ink/slate ramp for surfaces and borders, and a warm paper ramp for text — with two restrained route accents and two rarer status accents.

### Primary
- **Route Teal** (`teal`): pkg's own policy, state, and flow — the controlled backbone. Command prompts, active tabs, selected chips and stations, the transit map's main line, and the "go" action border.
- **Teal Deep** (`teal-deep`): the darker companion used for route borders and accent outlines where teal text or a stroke needs more weight than the fill.

### Secondary
- **Route Amber** (`amber`): the hidden Nix engine — fetch, substitute, NAR/signature verification, the store, and sandboxed builds. Terminal lines tagged `[nix]`, the build spur on the map, and engine pills.
- **Amber Deep** (`amber-deep`): borders and outlines for amber-coded elements and the engine-pill stroke.

### Tertiary
- **Signal Rust** (`rust`): restrained error, refusal, and the cancel/keep action. Never alarming.
- **Rust Deep** (`rust-deep`): the darker companion used for the cancel/keep border where rust needs more weight than the fill — the exact analogue of `teal-deep` and `amber-deep`.
- **Signal Green** (`green`): restrained success, used only on terminal success lines.

### Neutral
- **Ink** (`ink`): the deepest canvas; the page background.
- **Ink Raised** (`ink-raised`): raised panels, terminal headers, and pill/chip/control backgrounds.
- **Ink Card** (`ink-card`): the terminal body and chip interiors — the deepest interactive surface.
- **Ink Border** (`ink-border`): the terminal's card border.
- **Rule** (`rule`): the default 1px hairline for dividers, panel edges, and section seams.
- **Rule Strong** (`rule-strong`): the stronger hairline for control borders, key caps, and segmented dividers.
- **Paper** (`paper`): primary text and headings.
- **Paper Secondary** (`paper-secondary`): supporting prose and secondary labels.
- **Paper Tertiary** (`paper-tertiary`): metadata, tertiary labels, and dimmed terminal lines.

### Scrims (neutral overlays)
- **Float Scrim** (`scrim`, `rgba(0,0,0,.35)`): the diffuse shadow tint under surfaces that genuinely float — the terminal and the bottom switcher.
- **Inset Scrim** (`scrim-soft`, `rgba(0,0,0,.18)`): the faint inset darkening of the terminal prompt well.

### Named Rules
**The Three-Role Rule.** Every concept is colored by who owns it: neutral paper for user intent, route teal for pkg policy and state, route amber for the hidden Nix engine. When a surface shows more than one role, the three roles must stay visually separable.
**The Route Restraint Rule.** Teal and amber are route colors — semantic signals, never decoration. They live on lines, dots, signal nodes, role bars, command prompts, and narrow borders, not as fills across large areas. Rust and green are rarer still, reserved for terminal error and success lines.

## Typography

**Display/Body Font:** system sans (`-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif`)
**Mono Font:** system monospace (`ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", "DejaVu Sans Mono", monospace`)

**Character:** A restrained, utilitarian pairing that lets the OS native faces do the work. Sans carries prose and chrome at a modest scale (the largest heading is ~1.3rem); monospace carries everything that must be exact — commands, output, identifiers, versions, store paths, keyboard caps. The two faces never trade jobs.

### Hierarchy
- **Headline** (sans, 600, 1.32rem, 1.25): section titles, the primary statement of each view.
- **Title** (sans, 600, 1.12rem, 1.25): subsection, brand, and station headings.
- **Body** (sans, 400, 1rem, 1.55): running prose and descriptions; keep line length bounded (~60–70ch where set).
- **Lead** (sans, 400, 0.875rem, 1.55): secondary prose — section descriptions, station intent lines, role rows, and the mobile body step-down.
- **Label** (sans, 400, 0.74rem, uppercase, ~0.06em tracking): eyebrows, legend captions, panel headers, and status text.
- **Mono Data** (mono, 400, 0.82rem, 1.5): all terminal output, command chips, KV rows, key caps, and reference lists.

### Named Rules
**The Mono-for-Truth Rule.** Anything that is data, a command, output, an identifier, a version, a store path, or a keyboard key is monospace. Sans is for prose and chrome. Never set a package name, version, path, or command in the sans face.

## Layout

A single centered column capped at a 1180px container with 20px side padding (14px on small screens). Within it, surfaces use explicit, few-column grids rather than fluid card fields: the Guided Day is a sticky 168px rail beside a single scrolling column; the Command Desk is a fixed three-column workbench (248px families · fluid terminal · 268px context); the Lifecycle Map is a horizontally-scrolling SVG with a two-column detail region below. A floating, centered pill switcher is pinned to the bottom of the viewport and the main column reserves bottom padding so content never hides behind it.

Density is moderate and consistent: 14–18px panel padding, 6–8px internal gaps, and hairline rules in place of heavy containers. A very faint horizontal ruled-paper texture sits behind the header as atmosphere, not structure. Responsive behavior collapses the multi-column grids to a single column below 980px and tightens type and padding below 640px (legend stacks to one column, the keyboard hint hides, switcher labels collapse to keys).

## Elevation & Depth

Depth is tonal, not shadowed. The canvas steps from Ink down through Ink Raised (panels, terminal headers, controls) to Ink Card (terminal bodies, chip interiors), and edges are 1px hairlines in the Rule family. Resting surfaces are flat. The entire system uses a single shadow value — a low, diffuse scrim — and it is reserved for surfaces that genuinely float above the canvas: the terminal and the bottom switcher bar (the latter faintly blurred with a `backdrop-filter`). Nothing else is lifted.

### Shadow Vocabulary
- **Float Scrim** (`box-shadow: 0 10px 30px rgba(0,0,0,.35)`): terminals and the floating switcher only.

### Named Rules
**The Tonal-Depth Rule.** Depth is conveyed by stepping down the ink ramp (canvas → raised panel → card), not by shadow. The single box-shadow is reserved for surfaces that float above the canvas, and even there it is a low, diffuse scrim. Resting surfaces stay flat.

## Shapes

Corners are small and consistent: the canonical panel/terminal/station corner is 8px (`--radius`); command chips and terminal buttons are 6px; the segmented control and scenario buttons match the panel corner at 8px; key caps are 4px; pills and chips are fully round (999px); focus rings and role bars clip to a 3px radius; station signal nodes are round. The distinctive form language is the **round signal node** on each station's clock row — a small role-colored dot (teal for pkg, amber for the hidden engine, paper-secondary for recovery) that marks the beat's route without a heavy one-side card border. Otherwise lines are 1px hairlines, and the only thick strokes are the route lines on the transit map (4–9px).

### Named Rules
**The Hairline Rule.** Lines and borders are 1px hairlines in the rule family. The only thick strokes are the route lines on the transit map (4–9px); a station's role is marked by a small round signal node, not a thick edge. Avoid heavy outlines and double borders.

## Components

### Terminal (signature)
The centerpiece. A flat card (`ink-card`) with a 1px `ink-border`, an 8px corner, and the float scrim. A mono header shows faint "lights," a title, and an "illustrative" tag; a scrollable mono body renders color-coded lines (command, ok, warn, err, pkg, nix, dim); an optional prompt row holds a question plus a teal "go" and a rust "cancel" button; a footer carries state and replay. Every command/output line is monospace and color-tagged by role.

### Buttons
- **Shape:** 6px corner, mono face, 1px `rule-strong` border, `ink-raised` fill.
- **Go (primary action):** teal text and a `teal-deep` border; hover tints the fill faintly teal.
- **Cancel / Keep (safe action):** rust text and a `rust-deep` border; hover tints the fill faintly rust.
- **Neutral (replay, chips):** `paper-secondary` text on `ink-raised`; hover lifts to `ink-card`.
- **Hover / Focus:** 120ms color/border transition; keyboard focus uses the global 2px paper `:focus-visible` outline.

### Pill
A fully-round status badge (`ink-raised`, `paper-secondary`, uppercase ~0.72rem) with a 6px role dot (amber by default, teal on the `.teal` variant). Used for prototype/status flags, not metrics.

### Segmented Control
An 8px-cornered `ink-raised` group of buttons divided by 1px `rule` seams; the pressed segment fills with `ink-card` and lifts text to `paper`; each segment carries a 7px role dot (teal when pressed, dimmed `paper-tertiary` otherwise).

### Keyboard Key (kbd)
A mono ~0.72rem cap with a 1px `rule-strong` border, a 2px bottom edge (for a subtle key relief), 4px corner, on `ink-raised`. Pairs with a mono `kbd-hint` to advertise shortcuts; the hint hides on small screens.

### Panel
The generic container: `ink-raised`, 8px corner, 14px padding, 1px `rule` border, flat. Panel headers are uppercase ~0.82rem labels in `paper-tertiary`.

### Station (guided beat)
A flat `ink-raised` panel with an 8px corner whose **clock row carries a small round signal node** marking its role (teal for pkg beats, amber for Nix/build beats, `paper-secondary` for recovery). Holds a mono clock, a title, an intent line, role chips, an embedded terminal, and a note line. `scroll-margin-top` keeps anchored stations clear of the header.

### Chip
A fully-round mono tag (`ink-card`, `paper-secondary`, ~0.74rem). Route variants tint text and border: `.pkg` is teal, `.nix` is amber. Used to label the role of a command or beat.

### Role Row
A three-row legend: a 3px colored bar (paper = user intent, teal = pkg, amber = hidden engine) beside an uppercase label and a `paper-secondary` description. The recurring device that restates the Three-Role Rule inline under any station or context panel.

### Variant Switcher
A floating, centered, fully-round bar pinned to the viewport bottom, `rgba(ink-raised, .92)` with a faint backdrop blur and the float scrim. Each segment is a pill button with a mono key cap; the active segment fills with `ink-card`, lifts to `paper`, and turns its key cap teal. Labels collapse to keys on small screens.

### Named Rules
**The Calm-Refusal Rule.** Destructive or build actions never win by default. The safe action (Cancel / Keep) takes initial focus, and refusals read as calm statements in rust, not alarms. Errors explain the boundary and the next step; they never flash or shout.

## Do's and Don'ts

### Do:
- **Do** keep every concept separable into the three roles — user intent (paper), pkg (teal), hidden Nix engine (amber).
- **Do** set commands, output, identifiers, versions, store paths, and keyboard keys in the monospace face.
- **Do** give the safe action initial focus in any confirmation, and state refusals calmly in rust with a next step.
- **Do** convey depth by stepping the ink ramp; reach for the single diffuse shadow only on surfaces that truly float.
- **Do** keep keyboard focus visible (a 2px paper outline via `:focus-visible`) and honor `prefers-reduced-motion` everywhere.
- **Do** label illustrative numbers, sizes, and timings as illustrative — never present them as measured.

### Don't:
- **Don't** build a generic SaaS dashboard: no card grids of vanity metrics, no drop shadows under every panel, no busy chrome.
- **Don't** use neon "hacker terminal" clichés — no pure-black backgrounds, no bright green-on-black, no scanline or CRT FX, no glitch.
- **Don't** use gradients as decoration or gradient text; the only permitted gradient is a near-flat, same-hue tonal blend used as a depth seam.
- **Don't** add emoji or pictographic ornament; status is a dot or a monospace glyph, not an icon font.
- **Don't** present fabricated metrics, sizes, or output as real — label them illustrative.
- **Don't** clutter the canvas with stacked cards; prefer one terminal or map focus and quiet supporting panels.
