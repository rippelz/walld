# walld

Lightweight **Hyprland** wallpaper daemon for this rice: layer-shell surfaces +
EGL/GLES, optional diagonal wipe on switch, Unix-socket IPC. Not a general
toolkit and not a hyprpaper clone.

## What it does

1. One background layer-shell surface per monitor (static images)
2. Reloads paths when config changes or themes apply
3. Transitions: `snap` (instant) or `wipe` (GPU diagonal reveal)
4. Coexists with **hyprmotion** for video themes (`stop` / `start` handoff)


## Scenes (wallpaper engine prototype)

Scene documents are JSON (schema v1). Shared crate: `wallengine-scene`.

```bash
# load a scene on all monitors
walld ctl scene '*' ~/.local/share/wallengine/scenes/yozakura-snow/scene.json
walld ctl status   # shows scene=yozakura-snow (animated)

# example scenes ship in examples/scenes/ and install to:
#   ~/.local/share/wallengine/scenes/
```

Layer types so far: `image`, `color`, `particles` (presets: `snow`, `dust`).

Optional in `~/.config/walld/config`:

```
scene = ~/.local/share/wallengine/scenes/yozakura-snow/scene.json
scene_fps = 30
```

Classic `hyprpaper.conf` image mode still works when `scene` is unset.

## wallstudio (iced gallery)

```bash
cargo build -p wallstudio --release
install -Dm755 target/release/wallstudio ~/.local/bin/wallstudio
wallstudio   # gallery + apply scenes via walld IPC
```

Lists `~/.local/share/wallengine/scenes/*`, shows daemon status, Apply → `walld ctl scene`.

## Build / install

```bash
cd ~/code/walld
cargo build --release
install -Dm755 target/release/walld ~/.local/bin/walld
```

## Config

| Path | Role |
|------|------|
| `~/.config/walld/config` | Global options (`transition`, `wipe_ms`, …) |
| `~/.config/hypr/hyprpaper.conf` | Per-monitor `wallpaper { … }` blocks (hyprpaper-compatible; `theme apply` already merges this) |

Example `~/.config/walld/config`:

```
transition = wipe    # snap | wipe
wipe_ms = 480
wipe_feather_px = 80
```

`SIGHUP` reloads wallpaper paths using the configured transition (same as
`walld ctl reload`).

## IPC

Socket: `$XDG_RUNTIME_DIR/walld.sock`

```bash
walld ctl ping
walld ctl status
walld ctl reload
walld ctl set  <monitor|*> <path>   # default transition
walld ctl snap <monitor|*> <path>
walld ctl wipe <monitor|*> <path>
walld ctl preload <path>
walld ctl stop     # hide surfaces (video handoff)
walld ctl start    # recreate surfaces
walld ctl ready    # ok when every output has a committed frame
walld ctl quit
```

## Autostart (Hyprland)

This setup uses `exec-once = ~/.local/bin/wallpaper-boot`, which starts **walld**
for static themes and **hyprmotion** for video themes.

Optional user unit (example in-tree):

```bash
mkdir -p ~/.config/systemd/user
cp ~/code/walld/walld.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now walld.service
# If you still have hyprpaper enabled:
systemctl --user disable --now hyprpaper.service
```

Do **not** run hyprpaper and walld together for daily use — both paint
background layers.

## Theme integration

`~/.local/bin/theme` (and `~/.config/themes/bin/theme`) drives walld:

| Transition | Order |
|------------|--------|
| static → static | `merge_hyprpaper` → `walld reload` (wipe/snap from config) |
| video → static | walld `start` + `reload` + `ready` **under** video → kill mpvpaper |
| static → video | optional walld underlayer → hyprmotion start → `walld stop` |

Scripts can poll `walld ctl ready` instead of a fixed `sleep`.

## Video coexistence (hyprmotion)

- **Static themes** → walld shows images from hyprpaper.conf paths
- **Video themes** → walld **stop** (hide); hyprmotion / mpvpaper runs
- **Back to static** → walld **start** + set/reload path **before** killing video
  (avoids Hyprland default background flash)

## Manual smoke

```bash
# kill legacy static engine if still running
pkill -x hyprpaper || true
walld -v &
walld ctl status
walld ctl wipe DP-1 ~/Pictures/Wallpapers/ember.jpg
```
