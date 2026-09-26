# Experimental web wallpapers

Web projects run in a persistent, sandboxed headless Chromium process. Install
`chromium` on PATH. Local HTML, CSS, JavaScript, video and WebGL are rendered
through Chromium and uploaded as wallpaper textures. Each instance has its own
temporary browser profile, which is removed when it stops.

The bridge supplies Wallpaper Engine property defaults and saved user overrides
at page load. Editing properties reloads the wallpaper through the existing
daemon property API. Nested entrypoints and paths containing spaces or `#` work.
Missing entrypoints and browser startup failures are reported instead of silently
falling back to another HTML file.

## Current limits

- Mouse and keyboard events are not forwarded. Interactive menus cannot yet be used.
- System audio capture is not connected; audio listeners receive 128 silent bins.
  Audio-only visualizers may consequently remain black. Media listeners are stubs.
- Rendering uses a 16:9 viewport (1920×1080 by default), scaled onto the output.
  `WALLD_WEB_MAX_EDGE` changes the longest edge, clamped to 640–2560.
- PNG readback targets 15 fps; achieved rate depends on the page and GPU/CPU.
- Playback pause/rate controls are not implemented for web content.
- Browser storage is temporary and does not survive restarting the wallpaper.

This is initial rendering support, not full Wallpaper Engine web API compatibility.

## Verification

Run the real-browser regression test explicitly:

```sh
cargo test -p walld --test web_runtime -- --ignored
```

Capture a project without changing the desktop:

```sh
cargo run -p walld --example web_capture -- /path/to/index.html /tmp/web.png 10
```

The test checks initial property delivery, nested/escaped paths, persistent
JavaScript animation and two simultaneous browser instances.
