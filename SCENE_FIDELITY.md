# Scene fidelity invariants

Fix renderer behavior, never individual workshop scene coordinates or assets.

## Coordinates

Wallpaper Engine scene positions use bottom-left, Y-up coordinates. The
centered camera has the same axes: subtract half the canvas dimensions, without
reflecting Y. Only viewport/UI pixels use top-left, Y-down coordinates.

Images, puppet vertices, particle systems, parent transforms, and editor gizmos
must use this same convention. Preserve signed scales for mirrored layers.
Scene Z rotations keep their sign in camera space. Particle sprite rotations
change sign only when projected into viewport pixel space.

## Effects

`src/effects.rs` is the shared executor for the daemon and `gl_render_scene`.

- Each effect's `previous` binding captures the result of the preceding effect.
- Preserve that input throughout the effect. Never render into a sampled texture.
- Named intermediate buffers remain separate from the final layer output.
- Skip an incomplete effect atomically; do not read unwritten intermediate data.
- Normalize padded image storage into a content-sized buffer before effects.
- Sampler resolution is `(storage width, storage height, content width, content height)`.
- Only active shader samplers require a resolved texture. Conditional unused
  declarations must not prevent an otherwise complete effect from running.
- Dedicated opacity/waterflow/color-key paths are fallbacks, not a second
  application of effects that already succeeded in the generic chain.
- Puppet draws must initialize all uniforms and blending state they use.

GLSL translation must preserve both lexical scope and conditional compilation.
Hoisting local constants out of `#if` branches breaks stock shaders such as godrays.

## Verification

```sh
cargo test -p wallengine-we --lib --test scene_pipeline --test puppet_animation
cargo test -p walld --lib --bins --test scene_fidelity
cargo check -p walld -p wallstudio --lib --bins --tests
cargo run -p walld --example gl_render_scene -- WORKSHOP_ID /tmp/scene.png 960 540
```

The GPU regression requires EGL/GLES; Mesa surfaceless works. It uses synthetic
assets and checks actual pixels for chained effects, alias-free intermediate
buffers, failed-program handling, padded masks, and puppet draw-state isolation.
Coordinate regressions compare puppet and quad transforms under translation,
rotation, and reflection. The scene test checks authored positions and fallback
suppression without editing workshop files.

## Puppet animation

MDLS0004 / MDLA0006 puppets with 80-byte skinned vertices now play authored
`animationlayers` by clip ID. The shared runtime advances sampled TRS tracks,
composes the parent hierarchy, and skins all four vertex influences with
`animated_world * inverse(bind_world)`. Each frame starts from the original
vertices, so transforms cannot accumulate. Playback honors clip FPS, loop/closing
samples, layer rate, visibility, weight, and additive deltas from frame zero.
Unknown layouts or invalid skeleton/animation records retain the static mesh
with a diagnostic. No scene IDs, positions, or assets are overridden.

`puppet_animation` uses synthetic binary assets to verify hierarchy order,
inverse bind, weighted rotation/translation, clip IDs, layers, looping,
truncation/cycles, and integration through `WeSceneRuntime::tick`.

Known limitations remain: other skinned vertex layouts, animated attachment
sockets/attached limbs, scripted per-bone control and layer fade envelopes,
some custom GLSL numeric
type conversions, and complete cross-layer/backbuffer/perspective effect support.
The headless example shares effect execution but does not reproduce every live
daemon feature (notably compose layers and video playback); use the daemon's
editor preview for a final live check.

`cargo check --all-targets` also includes the existing `examples/boa_repro.rs`,
which imports `boa_engine` without a root-package dependency and currently fails.
That unrelated debug example is excluded by the explicit checks above.
