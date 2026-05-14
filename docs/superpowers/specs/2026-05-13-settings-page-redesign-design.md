# Settings page redesign

## Goal

Rework `/settings` to match the "Quiet terminal" design language used elsewhere
in the dashboard (sidebar, pings table, memory editor). The new page lifts
layout + components from `design_handoff_dashboard/settings.jsx` and the
matching block in `design_handoff_dashboard/styles.css`.

Scope is **visual only** — no new fields, no backend changes. The existing
backend (`crates/web/src/routes/settings.rs`) still drives cooldowns + pings,
unchanged. The new shell is structured so additional sections can drop in once
their `SettingsOverrides` fields exist.

## Out of scope

- The reference's Twitch / AI / Memory / Aviation sections (no backend
  support yet).
- New persistence behavior (atomic write, audit logging — already covered).
- Per-row reset *server endpoint* — keep `/settings/reset/{section}` only.

## Visual structure

```
┌─ page-head ──────────────────────────────────────────────────┐
│  Settings                                                    │
│  Runtime configuration. Saved values apply immediately…      │
├─ settings-grid (200px nav | 1fr main) ───────────────────────┤
│  ┌─ settings-nav ──┐ ┌─ settings-main ────────────────────┐  │
│  │ Sections         │ │ ┌─ settings-card ──────────────┐  │  │
│  │ • Cooldowns      │ │ │ Cooldowns       [cooldowns]  │  │  │
│  │ • Pings          │ │ ├──────────────────────────────┤  │  │
│  │                  │ │ │ • ai      [_15  s_] default… │  │  │
│  │                  │ │ │ • news    [_60  s_] default… │  │  │
│  │                  │ │ │ …                            │  │  │
│  │                  │ │ └──────────────────────────────┘  │  │
│  │                  │ │ ┌─ settings-card ──────────────┐  │  │
│  │                  │ │ │ Pings              [pings]   │  │  │
│  │                  │ │ │ …                            │  │  │
│  │                  │ │ └──────────────────────────────┘  │  │
│  │                  │ │ $DATA_DIR/config.toml · atomic     │  │
│  └──────────────────┘ └────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────────┤
│  ▸ save-bar (sticky, slides up when dirty>0)                 │
│    ● 2 changes pending  ai · public      [Discard] [Save]    │
└──────────────────────────────────────────────────────────────┘
```

### Section nav

- Sticky inside the page (`position: sticky; top: 12px`).
- Each item: `•` dot + section name + dirty count badge (`.ndirty`) when > 0.
- Active section: accent-tinted background, dot lit + glowing accent.
- Anchor `<a href="#sec-cooldowns">` for no-JS fallback. JS upgrades to:
  smooth scroll + IntersectionObserver active-state.

### Section card

- `<section id="sec-{slug}" class="settings-card">`
- Head row: `<h2>` title + 1-line blurb + right-side `<code>[slug]</code>` tag
  + dirty badge when count > 0.
- Body: `<div class="settings-rows">` containing N `.settings-row`.

### Row layout

Three columns: `1fr 240px 200px`.

- **row-left**: dirty dot + `<code>` key (mono) + secondary `row-hint` line.
- **row-control**: the input. Variants used today:
  - **toggle**: `<label class="toggle">` wrapping a hidden `<input
    type="checkbox">` and a `<span class="toggle-thumb">`. CSS uses
    `:has(input:checked)` to drive on-state. Submits as a normal checkbox.
  - **number with unit**: `<div class="num-wrap">` containing `<input
    type="number" class="num-input">` and `<span class="num-unit">s</span>`.
- **row-right**: `default <code>30s</code>` + per-row `↺` reset button (JS-only;
  hidden when row is not dirty).

### Save bar

Fixed at bottom of viewport, offset by sidebar (`left: var(--rail-w)`).
Hidden by default; `.visible` slides it up. Inside:

- Left: pulsing accent dot + `N changes pending` + first 3 changed keys.
- Right: **Discard** (ghost, reloads page) + **Save changes** (primary,
  submits the form via `form="settings-form"`).

The save-bar buttons live *outside* the `<form>` and use the HTML5 `form="…"`
attribute, so the bar can be a sibling without nesting forms.

### Reset section

Two small `<form>` blocks at the bottom of the template (outside the main
form) keep their existing `/settings/reset/{cooldowns,pings}` action. They're
re-positioned but the network shape doesn't change.

## Toggle

```html
<label class="toggle">
  <input type="checkbox" name="ping_public" value="1" {% if … %}checked{% endif %}>
  <span class="toggle-thumb"></span>
</label>
```

```css
.toggle { width:36px; height:20px; border-radius:999px;
          background:var(--bg-3); border:1px solid var(--line-2);
          position:relative; cursor:pointer;
          transition:background .12s ease, border-color .12s ease; }
.toggle input { position:absolute; inset:0; opacity:0; margin:0; cursor:pointer; }
.toggle .toggle-thumb { position:absolute; top:50%; left:2px;
          transform:translateY(-50%); width:14px; height:14px;
          border-radius:50%; background:var(--fg-3);
          transition:left .16s ease, background .16s ease; }
.toggle:has(input:checked) { background:var(--accent); border-color:transparent; }
.toggle:has(input:checked) .toggle-thumb { left:calc(100% - 16px);
          background:var(--accent-ink); }
.toggle:has(input:focus-visible) { box-shadow:0 0 0 3px var(--accent-soft); }
```

Browser support: `:has()` is in all evergreen targets (Chrome 105, FF 121,
Safari 15.4). Acceptable for a moderator-only dashboard.

## JS

New IIFE appended to `assets/app.js`, guarded by presence of `#settings-form`.

Responsibilities:

1. **Dirty tracking.** Each input carries `data-default="…"`. On `input`/`change`
   compare current value (`.checked` for checkboxes) to `data-default`, toggle
   `.is-dirty` on the enclosing `.settings-row`.
2. **Section dirty counts.** Aggregate `.is-dirty` per `.settings-card`,
   update `.card-dirty` badge in the card head and `.ndirty` badge in the
   nav.
3. **Save-bar.** Total dirty count → toggle `.save-bar.visible`, update
   `<strong>` count and `.muted` 3-key preview.
4. **Section nav active state.** IntersectionObserver across `.settings-card`
   elements, add `.active` on the matching `.settings-nav-item`.
5. **Per-row reset (↺).** Click sets the input value back to `data-default`,
   dispatches an `input` event so dirty tracking re-runs.
6. **Discard.** Button reloads the page (`location.reload()`), drops local
   edits.

No fetch / no client-side persistence. Save still goes through the existing
form POST.

## Files

- `crates/web/assets/app.css` — append new section (`/* === Settings === */`).
  Old `.card > label.check`, `.form-actions`, `.card-row`, `.reset-form` rules
  for the settings page no longer needed; remove if not used elsewhere.
- `crates/web/templates/settings.html` — full rewrite per layout above.
- `crates/web/assets/app.js` — append settings IIFE.
- `crates/web/tests/settings_route.rs` — adjust the strict negative-string
  assertion in `validation_error_renders_form_with_errors` so it survives
  attribute-order/format changes (use a windowed substring check around
  `name="cooldown_news"`).

## Out of scope but adjacent

- The reference `settings.jsx` includes optimistic per-section dirty counts
  and an unused "Open config.toml" action. Skip the action (no endpoint).
  Keep the dirty counts.
