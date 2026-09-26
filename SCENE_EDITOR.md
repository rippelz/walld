# wallstudio Scene Editor — working checklist

Living checklist for the workshop-first WE scene editor.
Update status as work lands (`[ ]` → `[x]`). Do **not** invent a second renderer.

## Locked decisions

| # | Decision |
|---|----------|
| 1 | Editor opens as a **new window**, still **wallstudio** (iced multi-window / daemon) |
| 2 | Live preview uses the **same walld WE pipeline** as the desktop wallpaper |
| 3 | Source of truth: **WE JSON + package tree** (cross-compatible with Wallpaper Engine) |
| 4 | Workshop edit is **FULL** (transforms, effects params, particle overrides, texture replace) |
| 5 | **New blank scenes only after workshop edit is solid** |
| 6 | When blank scenes ship: **user-chosen resolution** (not hard-coded 1920×1080) |
| 7 | Never write into Steam workshop dirs — always **fork** to local projects |

## Project layout (forks)

```
~/.local/share/wallengine/projects/<fork-id>/
  project.json          # title, type=scene, file=scene.json, forked_from
  scene.json            # editable WE scene (no scene.pkg while editing)
  materials/ models/ particles/ effects/ …
  preview.jpg
```

`scene.pkg` is import-only; re-pack is a later export step so walld always reads live `scene.json`.

---

## Phase 0 — Foundations

- [x] Write this checklist
- [x] `wallengine` projects dir path + scan in library (LocalFolder includes forks)
- [x] Fork workshop item → unpack `scene.pkg` → copy tree **without** `scene.pkg`
- [x] `EditableScene`: load/save `scene.json` preserving unknown fields
- [x] Layer summaries (id, name, kind, parent, visible, scripted badges)
- [x] wallstudio → `iced::daemon` multi-window (library + editor windows)
- [x] “Edit scene” entry point from library detail (Scene only)
- [x] Editor chrome: toolbar · layer tree · preview status · inspector
- [x] Preview: **GPU via walld** (`we_editor load` — same shaders as wallpaper, offscreen PNG)
- [x] Hot-reload after save / after edit (auto-preview)
- [x] Rename / delete local projects in library

## Phase 1 — P0 editing (workshop useful)

### Document ops
- [x] Dirty flag, Save
- [ ] Save As (optional rename id)
- [x] Undo / Redo (document snapshots)
- [ ] Ctrl+Z / Ctrl+Shift+Z / Ctrl+S in editor window (toolbar works)

### Layer tree
- [x] Select layer
- [x] Toggle visibility
- [x] Rename
- [x] Duplicate / Delete
- [x] Reorder (↑/↓ buttons; drag later)
- [x] Show hierarchy indent from `parent`
- [x] Badges: scripted · animated · effect

### Inspector — transform / object
- [x] Origin X/Y
- [x] Scale X/Y
- [x] Angles Z (degrees in UI → radians on disk)
- [x] Alpha, brightness
- [ ] Color picker (read API exists; UI later)
- [ ] Blend mode / alignment
- [x] Image: model path display
- [x] Particle: path + instanceoverride (rate, speed, size, alpha, count, lifetime)
- [x] Text: literal display

### Inspector — effects
- [x] List effect instances on selected layer
- [x] Toggle effect visible
- [x] Edit float `constantshadervalues`
- [ ] Vec / color constants
- [ ] Combos read-only

### Assets
- [x] Package file browser (list relative paths)
- [ ] Replace image texture / model path via picker
- [x] Import external image into `materials/imported/`
- [ ] Wire imported image to layer model

### Preview / apply
- [x] Play / Stop from editor toolbar
- [x] Monitor picker
- [x] Auto-reload walld after edit (save + `walld ctl we`)
- [x] Status line

## Phase 2 — P1 depth

- [ ] Particle system JSON editor (emitters / operators you already parse)
- [ ] Timeline keyframes for alpha / origin (view + basic edit)
- [ ] User-properties authoring in `project.json` (`general.properties`)
- [ ] Bind user props → layer/effect fields (document pattern)
- [ ] Orphan asset report
- [ ] Diagnostics panel (layer count, particle count, effect passes)

## Phase 3 — Create new scenes (only after workshop solid)

- [ ] New project wizard
- [ ] **Resolution picker** (presets + custom W×H → ortho width/height)
- [ ] Templates: blank, image + particles, clone structure from workshop
- [ ] Re-pack `scene.pkg` for sharing / WE open
- [ ] Generate `preview.jpg` from walld frame or soft render

## Phase 4 — Power

- [ ] SceneScript panel + known bindings
- [ ] Merge / re-fork when Steam updates upstream workshop id
- [ ] Puppet bone tools (beyond visibility)
- [ ] Shader effect authoring (only if needed)

## Explicit non-goals (for now)

- Editing Steam workshop tree in place
- Web / Application wallpapers
- Steam Workshop upload
- Separate `walleditor` binary
- Second “lite” native scene format as the editor native (WE format wins)

## Vertical slice acceptance (ship gate for Phase 0+1)

1. Open a subscribed Scene wallpaper → Edit scene → new window
2. Fork appears under library after refresh
3. Select a layer → change origin / alpha / particle rate / effect constant
4. Save → walld wallpaper matches the edit (same pipeline)
5. Close editor; re-open fork; edits still there
6. Original workshop item untouched

## Implementation map

| Area | Crate / files |
|------|----------------|
| Paths, fork, editable JSON | `wallengine-we` (`paths`, `editor`, `pkg`, `scan`) |
| Multi-window shell + library entry | `wallstudio` (`main`, `ui`) |
| Editor UI + commands | `wallstudio` (`editor`) |
| Preview | `wallengine-we::player` → `walld ctl we` |
| Checklist | `SCENE_EDITOR.md` (this file) |

## Notes

- WE stores many vectors as strings: `"2048.000 1152.000 0.000"`. Mutators must round-trip that form when the field was a string, and objects when it was `{ value: … }` / timeline.
- Angles: static scene.json often stores **radians**; UI should show degrees and convert.
- If both `scene.pkg` and `scene.json` exist, walld prefers **unpacking the pkg**. Forks must drop `scene.pkg` while editing.
- Soft-render examples stay for CI/debug; **not** the editor viewport.
