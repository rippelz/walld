//! SceneScript host: executes the actual JavaScript that Wallpaper Engine
//! scenes attach to object properties (origin/angles/scale/alpha/visible/
//! text/color), against a live mirror of the scene graph.
//!
//! Model: every graph node is mirrored as a JS object in `__layersById`.
//! Each scripted property is compiled once (its `export`s stripped, its
//! `scriptProperties` resolved from project user properties). Per tick the
//! host runs every script's `update(value)` with `thisLayer`/`thisScene`/
//! `engine`/`shared` set up like WE, then serializes the mutated graph state
//! back to Rust. Input/media events don't exist on a wallpaper daemon, so
//! event handlers (cursorClick, mediaPlaybackChanged, …) are never fired —
//! matching an idle WE session.

use crate::scene::model::GraphNode;
use boa_engine::{Context, Source};
use serde_json::Value;
use std::collections::HashMap;

pub struct ScriptHost {
    ctx: Context,
    /// Nodes that have at least one scripted property.
    pub scripted_nodes: usize,
    /// Set after a Boa internal panic so we stop calling into JS. A Rust panic
    /// in boa cannot be caught by the JS `try/catch` around `update`/`init`,
    /// and would otherwise kill the whole wallpaper daemon (seen on the Jett
    /// scene's audio-visualizer: `must be declarative environment`).
    disabled: bool,
}

/// Per-node state read back from JS after a tick.
#[derive(Debug, serde::Deserialize)]
pub struct NodeScriptState {
    pub id: i64,
    /// Local origin (screen px, parent-relative; absolute for dynamic bars).
    pub o: [f32; 3],
    /// Angles in degrees (script convention).
    pub an: [f32; 3],
    pub s: [f32; 3],
    pub a: f32,
    pub v: bool,
    #[serde(default)]
    pub t: Option<String>,
    /// Animation playback rate set via getAnimation().rate.
    #[serde(default = "one")]
    pub r: f32,
    /// Set for layers created by `thisScene.createLayer(path)` (e.g. audio bars).
    #[serde(default)]
    pub template: Option<String>,
    /// Layer color (RGB 0..1) when scripts assign `layer.color`.
    #[serde(default)]
    pub color: Option<[f32; 3]>,
}

fn one() -> f32 {
    1.0
}

const SCRIPTED_PROPS: [&str; 8] = [
    "origin", "angles", "scale", "alpha", "visible", "text", "color", "size",
];

impl ScriptHost {
    /// Build a host for the scene. Returns None when nothing is scripted.
    pub fn new(
        graph: &[GraphNode],
        raw_objects: &[Value],
        user_props: &HashMap<String, Value>,
        ortho: [f32; 2],
    ) -> Option<Self> {
        let any_scripts = raw_objects.iter().any(|o| {
            SCRIPTED_PROPS
                .iter()
                .any(|p| o.get(p).map(|v| v.get("script").is_some()).unwrap_or(false))
        });
        if !any_scripts {
            return None;
        }

        let mut ctx = Context::default();
        let tz_off_min = tz_offset_minutes();

        // Layer mirrors: id/name/local transform (angles in degrees for JS).
        // Include authored `color` so audio-visualizer bars inherit the parent tint.
        let color_by_id: HashMap<i64, [f32; 3]> = raw_objects
            .iter()
            .filter_map(|o| {
                let id = o.get("id")?.as_i64()?;
                let c = o.get("color")?;
                let rgb = if let Some(s) = c.as_str() {
                    let p: Vec<f32> = s
                        .split_whitespace()
                        .filter_map(|t| t.parse().ok())
                        .collect();
                    if p.len() >= 3 {
                        [p[0], p[1], p[2]]
                    } else {
                        return None;
                    }
                } else if let Some(a) = c.as_array() {
                    [
                        a.first()?.as_f64()? as f32,
                        a.get(1)?.as_f64()? as f32,
                        a.get(2)?.as_f64()? as f32,
                    ]
                } else {
                    return None;
                };
                Some((id, rgb))
            })
            .collect();
        let layers_json = serde_json::to_string(
            &graph
                .iter()
                .map(|n| {
                    let c = color_by_id.get(&n.id).copied().unwrap_or([1.0, 1.0, 1.0]);
                    serde_json::json!({
                        "id": n.id,
                        "name": n.name,
                        "o": n.local_origin,
                        "an": [
                            n.local_angles[0].to_degrees(),
                            n.local_angles[1].to_degrees(),
                            n.local_angles[2].to_degrees(),
                        ],
                        "s": n.local_scale,
                        "a": n.alpha,
                        "v": n.visible,
                        "c": c,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .ok()?;
        // All user properties as {name: value} for applyUserProperties.
        let mut flat_props = serde_json::Map::new();
        for (k, v) in user_props {
            let val = v.get("value").unwrap_or(v).clone();
            flat_props.insert(k.clone(), val);
        }
        let props_json = serde_json::to_string(&Value::Object(flat_props)).ok()?;

        let bootstrap = format!(
            r#"
var __tzOffMin = {tz_off_min};
(function() {{
    var off = __tzOffMin * 60000;
    function L(d) {{ return new Date(d.getTime() + off); }}
    Date.prototype.getHours = function() {{ return L(this).getUTCHours(); }};
    Date.prototype.getMinutes = function() {{ return L(this).getUTCMinutes(); }};
    Date.prototype.getSeconds = function() {{ return L(this).getUTCSeconds(); }};
    Date.prototype.getMilliseconds = function() {{ return L(this).getUTCMilliseconds(); }};
    Date.prototype.getDay = function() {{ return L(this).getUTCDay(); }};
    Date.prototype.getDate = function() {{ return L(this).getUTCDate(); }};
    Date.prototype.getMonth = function() {{ return L(this).getUTCMonth(); }};
    Date.prototype.getFullYear = function() {{ return L(this).getUTCFullYear(); }};
    Date.prototype.getTimezoneOffset = function() {{ return -__tzOffMin; }};
}})();

var console = {{ log: function(){{}}, warn: function(){{}}, error: function(){{}} }};
var shared = {{}};

// WE built-in modules/types (scripts `import` them; imports are stripped).
function Vec3(x, y, z) {{
    if (!(this instanceof Vec3)) return new Vec3(x, y, z);
    this.x = x || 0; this.y = y || 0; this.z = z || 0;
}}
Vec3.prototype.copy = function() {{ return new Vec3(this.x, this.y, this.z); }};
Vec3.prototype.add = function(v) {{ return new Vec3(this.x + v.x, this.y + v.y, this.z + v.z); }};
Vec3.prototype.subtract = function(v) {{ return new Vec3(this.x - v.x, this.y - v.y, this.z - v.z); }};
Vec3.prototype.multiply = function(s) {{ return new Vec3(this.x * s, this.y * s, this.z * s); }};
Vec3.prototype.divide = function(s) {{ return new Vec3(this.x / s, this.y / s, this.z / s); }};
Vec3.prototype.length = function() {{ return Math.sqrt(this.x*this.x + this.y*this.y + this.z*this.z); }};
Vec3.prototype.normalize = function() {{ var l = this.length() || 1; return new Vec3(this.x/l, this.y/l, this.z/l); }};
function Vec2(x, y) {{
    if (!(this instanceof Vec2)) return new Vec2(x, y);
    this.x = x || 0; this.y = y || 0;
}}
Vec2.prototype.copy = function() {{ return new Vec2(this.x, this.y); }};
Vec2.prototype.add = function(v) {{ return new Vec2(this.x + v.x, this.y + v.y); }};
Vec2.prototype.subtract = function(v) {{ return new Vec2(this.x - v.x, this.y - v.y); }};
Vec2.prototype.multiply = function(s) {{ return new Vec2(this.x * s, this.y * s); }};
Vec2.prototype.length = function() {{ return Math.sqrt(this.x*this.x + this.y*this.y); }};
Vec2.prototype.normalize = function() {{ var l = this.length() || 1; return new Vec2(this.x/l, this.y/l); }};
var WEMath = {{
    mix: function(a, b, t) {{ return a + (b - a) * t; }},
    lerp: function(a, b, t) {{ return a + (b - a) * t; }},
    clamp: function(v, a, b) {{ return Math.min(Math.max(v, a), b); }},
    saturate: function(v) {{ return Math.min(Math.max(v, 0), 1); }},
    frac: function(v) {{ return v - Math.floor(v); }},
    smoothStep: function(a, b, t) {{
        t = Math.min(Math.max((t - a) / (b - a), 0), 1);
        return t * t * (3 - 2 * t);
    }},
    random: function(a, b) {{
        if (a === undefined) return Math.random();
        if (b === undefined) return Math.random() * a;
        return a + Math.random() * (b - a);
    }},
    randomInt: function(a, b) {{ return Math.floor(WEMath.random(a, b + 1)); }},
    degToRad: function(d) {{ return d * Math.PI / 180; }},
    radToDeg: function(r) {{ return r * 180 / Math.PI; }},
}};
var __allProps = {props_json};
var engine = {{
    timeOfDay: 0,
    frametime: 1 / 30,
    runtime: 0,
    time: 0,
    canvasSize: {{ x: {ow}, y: {oh} }},
    screenSize: {{ x: {ow}, y: {oh} }},
    resolution: {{ x: {ow}, y: {oh} }},
    audioVolume: 0,
    isPaused: false,
    isRotating: false,
    isSilent: false,
    userProperties: __allProps,
    isScreensaver: false,
    registerUpdateCallback: function(){{}},
    unregisterUpdateCallback: function(){{}},
    // Audio reactivity: buffers are filled from the system capture each tick
    // (all-zero while silent, exactly like WE with no sound playing).
    AUDIO_RESOLUTION_16: 16,
    AUDIO_RESOLUTION_32: 32,
    AUDIO_RESOLUTION_64: 64,
    AUDIO_RESOLUTION_128: 128,
    registerAudioBuffers: function(res) {{
        res = res || 16;
        var b = {{ resolution: res, average: [], left: [], right: [], peak: 0, volume: 0 }};
        for (var i = 0; i < res; i++) {{ b.average.push(0); b.left.push(0); b.right.push(0); }}
        __audioBuffers.push(b);
        return b;
    }},
    setTimeout: function(fn, ms) {{
        var h = ++__timerSeq;
        __timers.push({{ id: h, fn: fn, due: engine.runtime + (ms || 0) / 1000, interval: 0 }});
        return h;
    }},
    setInterval: function(fn, ms) {{
        var h = ++__timerSeq;
        var s = (ms || 0) / 1000;
        __timers.push({{ id: h, fn: fn, due: engine.runtime + s, interval: s }});
        return h;
    }},
    clearTimeout: function(h) {{ __clearTimer(h); }},
    clearInterval: function(h) {{ __clearTimer(h); }},
}};
var __audioBuffers = [];
var __timers = [];
var __timerSeq = 0;
function __clearTimer(h) {{
    for (var i = 0; i < __timers.length; i++)
        if (__timers[i].id === h) {{ __timers.splice(i, 1); return; }}
}}
function setTimeout(fn, ms) {{ return engine.setTimeout(fn, ms); }}
function setInterval(fn, ms) {{ return engine.setInterval(fn, ms); }}
function clearTimeout(h) {{ __clearTimer(h); }}
function clearInterval(h) {{ __clearTimer(h); }}
var input = {{ cursorPosition: {{ x: {cx}, y: {cy} }}, cursorWorldPosition: {{ x: {cx}, y: {cy} }} }};

var __layersById = {{}};
var __layersByName = {{}};
var __layerOrder = [];
(function(list) {{
    for (var i = 0; i < list.length; i++) {{
        var n = list[i];
        var L = {{
            id: n.id,
            name: n.name,
            origin: {{ x: n.o[0], y: n.o[1], z: n.o[2] }},
            angles: {{ x: n.an[0], y: n.an[1], z: n.an[2] }},
            scale: {{ x: n.s[0], y: n.s[1], z: n.s[2] }},
            alpha: n.a,
            visible: n.v,
            text: '',
            __destroyed: false,
            __animRate: 1,
        }};
        L.getAnimation = (function(l) {{
            return function() {{
                if (!l.__animHandle) {{
                    l.__animHandle = {{
                        rate: 1, frame: 0, duration: 0, playing: true, looping: true,
                        play: function(){{ this.playing = true; }},
                        pause: function(){{ this.playing = false; }},
                        stop: function(){{ this.playing = false; this.frame = 0; }},
                        setFrame: function(f){{ this.frame = f; }},
                    }};
                }}
                return l.__animHandle;
            }};
        }})(L);
        // Spritesheet playback handle (rate is applied to frame timing).
        L.getTextureAnimation = (function(l) {{
            return function() {{
                if (!l.__texAnim) {{
                    l.__texAnim = {{
                        rate: 1, frame: 0, frameCount: 0, playing: true, looping: true,
                        play: function(){{ this.playing = true; }},
                        pause: function(){{ this.playing = false; }},
                        stop: function(){{ this.playing = false; this.frame = 0; }},
                        setFrame: function(f){{ this.frame = f; }},
                        getFrame: function(){{ return this.frame; }},
                    }};
                }}
                return l.__texAnim;
            }};
        }})(L);
        // Video texture handle. Host keeps `time`/`duration` fresh each tick
        // (see ScriptHost::sync_video_state). play/pause/seek set flags the
        // Rust decoder reads after the script tick.
        L.getVideoTexture = (function(l) {{
            return function() {{
                if (!l.__video) {{
                    l.__video = {{
                        playing: true, loop: true, volume: 0, time: 0, duration: 0,
                        __seek: null,
                        __endedCbs: [],
                        play: function(){{ this.playing = true; }},
                        pause: function(){{ this.playing = false; }},
                        stop: function(){{ this.playing = false; this.time = 0; this.__seek = 0; }},
                        setVolume: function(v){{ this.volume = v; }},
                        setCurrentTime: function(t){{ this.time = t; this.__seek = t; }},
                        getCurrentTime: function(){{ return this.time; }},
                        isPlaying: function(){{ return !!this.playing; }},
                        addEndedCallback: function(fn){{ if (typeof fn === 'function') this.__endedCbs.push(fn); }},
                    }};
                }}
                return l.__video;
            }};
        }})(L);
        L.getName = (function(l) {{ return function() {{ return l.name; }}; }})(L);
        L.getSize = (function(l) {{ return function() {{ return l.size; }}; }})(L);
        L.size = {{ x: 0, y: 0 }};
        // WE color property (Vec3). Scripts copy `bar.color = thisLayer.color`.
        L.color = {{ x: n.c[0], y: n.c[1], z: n.c[2] }};
        __layersById[n.id] = L;
        __layerOrder.push(L);
        if (!(n.name in __layersByName)) __layersByName[n.name] = L;
    }}
}})({layers_json});

var __nextDynId = -10000;
var thisScene = {{
    camerashake: false,
    camerashakespeed: 0,
    camerashakeamplitude: 0,
    // WE accepts a layer name, a scene-order index, or a layer object.
    getLayer: function(n) {{
        var L = null;
        if (typeof n === 'number') L = __layerOrder[n | 0];
        else if (typeof n === 'string') L = __layersByName[n];
        else L = n;
        return (L && !L.__destroyed) ? L : null;
    }},
    getLayerCount: function() {{ return __layerOrder.length; }},
    getLayerIndex: function(l) {{
        if (typeof l === 'string') l = __layersByName[l];
        for (var i = 0; i < __layerOrder.length; i++)
            if (__layerOrder[i] === l) return i;
        return -1;
    }},
    // Insert `layer` just above `atIndex` in draw order (WE audio visualizer).
    sortLayer: function(layer, atIndex) {{
        if (!layer) return;
        var from = -1;
        for (var i = 0; i < __layerOrder.length; i++)
            if (__layerOrder[i] === layer) {{ from = i; break; }}
        if (from < 0) return;
        __layerOrder.splice(from, 1);
        var to = (typeof atIndex === 'number') ? atIndex : __layerOrder.length;
        if (to < 0) to = 0;
        if (to > __layerOrder.length) to = __layerOrder.length;
        __layerOrder.splice(to, 0, layer);
    }},
    setLayerIndex: function(layer, idx) {{ thisScene.sortLayer(layer, idx); }},
    destroyLayer: function(l) {{
        if (typeof l === 'number') l = __layerOrder[l | 0];
        else if (typeof l === 'string') l = __layersByName[l];
        if (l) l.__destroyed = true;
    }},
    // Real dynamic layer (audio-visualizer bars, etc.). Registered so Rust can
    // materialize ImageLayers and scripts can drive origin/scale/angles.
    createLayer: function(path) {{
        var id = __nextDynId--;
        var p = path || '';
        // full-pixel / half-pixel templates are 1×2 unit quads.
        var isBar = /pixel/i.test(p);
        var L = {{
            id: id,
            name: p || ('dyn_' + id),
            __destroyed: false,
            __dynamic: true,
            __template: p,
            origin: {{ x: 0, y: 0, z: 0 }},
            angles: {{ x: 0, y: 0, z: 0 }},
            scale: {{ x: 1, y: 1, z: 1 }},
            size: {{ x: 1, y: isBar ? 2 : 0 }},
            alpha: 1,
            visible: true,
            text: '',
            color: {{ x: 1, y: 1, z: 1 }},
            alignment: 'center',
            perspective: false,
            parallaxDepth: {{ x: 0, y: 0 }},
            getName: function() {{ return this.name; }},
            getSize: function() {{ return this.size; }},
            getAnimation: function() {{
                if (!this.__animHandle) {{
                    this.__animHandle = {{
                        rate: 1, frame: 0, duration: 0, playing: true, looping: true,
                        play: function(){{ this.playing = true; }},
                        pause: function(){{ this.playing = false; }},
                        stop: function(){{ this.playing = false; this.frame = 0; }},
                        setFrame: function(f){{ this.frame = f; }},
                    }};
                }}
                return this.__animHandle;
            }},
            getTextureAnimation: function() {{ return this.getAnimation(); }},
            getVideoTexture: function() {{
                return {{ play: function(){{}}, pause: function(){{}}, stop: function(){{}}, setVolume: function(){{}} }};
            }},
        }};
        __layersById[id] = L;
        __layerOrder.push(L);
        return L;
    }},
    enumerateLayers: function() {{
        var out = [];
        for (var i = 0; i < __layerOrder.length; i++)
            if (!__layerOrder[i].__destroyed) out.push(__layerOrder[i]);
        return out;
    }},
}};

function createScriptProperties() {{
    var props = {{}};
    function grab(o, dflt) {{
        props[o.name] = (o.value !== undefined) ? o.value
            : (o.options && o.options.length ? o.options[0].value : dflt);
    }}
    var b = {{
        addSlider: function(o) {{ grab(o, 0); return b; }},
        addCheckbox: function(o) {{ grab(o, false); return b; }},
        addCombo: function(o) {{ grab(o, ''); return b; }},
        addText: function(o) {{ grab(o, ''); return b; }},
        addTextInput: function(o) {{ grab(o, ''); return b; }},
        addColor: function(o) {{ grab(o, '1 1 1'); return b; }},
        finish: function() {{
            var ov = globalThis.__scriptPropOverrides || {{}};
            for (var k in ov) props[k] = ov[k];
            return props;
        }},
    }};
    return b;
}}

var __scripts = [];
function __register(id, prop, overrides, src) {{
    src = src
        .replace(/^\s*import\s+[^;\n]*;?\s*$/mg, '')
        .replace(/export\s+function/g, 'function')
        .replace(/export\s+(var|let|const)/g, '$1')
        .replace(/export\s+default\s+/g, '')
        // Boa 0.20 can panic (not throw) on lexical bindings inside
        // `new Function` bodies: PutLexicalValue → "must be declarative
        // environment". WE SceneScripts never need TDZ semantics, so
        // demote let/const → var for a reliable host.
        .replace(/\blet\b/g, 'var')
        .replace(/\bconst\b/g, 'var')
        // Strict mode + var redecl of function params/names can also trip
        // Boa's environment stack; drop the directive.
        .replace(/(^|\n)\s*['\"]use strict['\"];?\s*/g, '$1');
    var body = src + "\n;return {{" +
        "update: (typeof update !== 'undefined') ? update : null," +
        "init: (typeof init !== 'undefined') ? init : null," +
        "applyUserProperties: (typeof applyUserProperties !== 'undefined') ? applyUserProperties : null }};";
    globalThis.__scriptPropOverrides = overrides;
    globalThis.thisLayer = __layersById[id] || null;
    globalThis.thisObject = globalThis.thisLayer;
    var h;
    try {{
        h = (new Function(body))();
    }} catch (e) {{
        return 'compile ' + id + '/' + prop + ': ' + e;
    }}
    __scripts.push({{ id: id, prop: prop, h: h, inited: false }});
    return '';
}}

function __tick(dt, tod, runtime, audio) {{
    engine.frametime = dt;
    engine.timeOfDay = tod;
    engine.runtime = runtime;
    engine.time = runtime;

    // Fill registered audio buffers from the capture (resampled per buffer).
    if (audio && audio.length) {{
        for (var bi = 0; bi < __audioBuffers.length; bi++) {{
            var b = __audioBuffers[bi];
            var peak = 0, sum = 0;
            for (var i = 0; i < b.resolution; i++) {{
                var v = audio[Math.floor(i * audio.length / b.resolution)] || 0;
                b.average[i] = v; b.left[i] = v; b.right[i] = v;
                if (v > peak) peak = v;
                sum += v;
            }}
            b.peak = peak;
            b.volume = sum / Math.max(b.resolution, 1);
        }}
    }}

    // Timers (setTimeout/setInterval).
    for (var ti = __timers.length - 1; ti >= 0; ti--) {{
        var tm = __timers[ti];
        if (runtime >= tm.due) {{
            try {{ tm.fn(); }} catch (e) {{}}
            if (tm.interval > 0) tm.due = runtime + tm.interval;
            else __timers.splice(ti, 1);
        }}
    }}
    for (var i = 0; i < __scripts.length; i++) {{
        var s = __scripts[i];
        var L = __layersById[s.id];
        if (!L || L.__destroyed) continue;
        globalThis.thisLayer = L;
        globalThis.thisObject = L;
        try {{
            if (!s.inited) {{
                s.inited = true;
                if (s.h.init) s.h.init();
                if (s.h.applyUserProperties) s.h.applyUserProperties(__allProps);
            }}
            if (s.h.update) {{
                var r = s.h.update(L[s.prop]);
                if (r !== undefined && r !== null) L[s.prop] = r;
            }}
        }} catch (e) {{}}
    }}
    var out = [];
    var num = function(x, d) {{ return (typeof x === 'number' && isFinite(x)) ? x : d; }};
    function readColor(c) {{
        if (!c) return undefined;
        if (typeof c === 'string') {{
            var p = c.trim().split(/\\s+/);
            if (p.length >= 3) return [num(+p[0], 1), num(+p[1], 1), num(+p[2], 1)];
            return undefined;
        }}
        // Vec3-like: x/y/z or r/g/b
        var r = num(c.x !== undefined ? c.x : c.r, 1);
        var g = num(c.y !== undefined ? c.y : c.g, 1);
        var b = num(c.z !== undefined ? c.z : c.b, 1);
        return [r, g, b];
    }}
    for (var k in __layersById) {{
        var n = __layersById[k];
        out.push({{
            id: n.id,
            o: [num(n.origin.x, 0), num(n.origin.y, 0), num(n.origin.z, 0)],
            an: [num(n.angles.x, 0), num(n.angles.y, 0), num(n.angles.z, 0)],
            s: [num(n.scale.x, 1), num(n.scale.y, 1), num(n.scale.z, 1)],
            a: num(n.alpha, 1),
            v: !!n.visible && !n.__destroyed,
            t: (typeof n.text === 'string' && n.text.length) ? n.text : undefined,
            r: n.__animHandle ? num(n.__animHandle.rate, 1) : 1,
            template: n.__dynamic ? (n.__template || n.name || undefined) : undefined,
            color: readColor(n.color),
        }});
    }}
    return JSON.stringify(out);
}}
"#,
            ow = ortho[0],
            oh = ortho[1],
            cx = ortho[0] * 0.5,
            cy = ortho[1] * 0.5,
        );

        if let Err(e) = ctx.eval(Source::from_bytes(bootstrap.as_bytes())) {
            log::warn!("script host bootstrap failed: {e}");
            return None;
        }

        // Register every scripted property.
        let mut scripted_nodes = 0usize;
        for obj in raw_objects {
            let Some(id) = obj.get("id").and_then(|v| v.as_i64()) else {
                continue;
            };
            let mut node_had = false;
            for prop in SCRIPTED_PROPS {
                let Some(pv) = obj.get(prop) else { continue };
                let Some(src) = pv.get("script").and_then(|v| v.as_str()) else {
                    continue;
                };
                // scriptproperties bindings: {name: {user, value} | literal}.
                let mut overrides = serde_json::Map::new();
                if let Some(Value::Object(sp)) = pv.get("scriptproperties") {
                    for (k, v) in sp {
                        let resolved = if let Some(user) =
                            v.get("user").and_then(|u| u.as_str())
                        {
                            user_props
                                .get(user)
                                .map(|p| p.get("value").unwrap_or(p).clone())
                                .or_else(|| v.get("value").cloned())
                        } else {
                            v.get("value").cloned().or_else(|| Some(v.clone()))
                        };
                        if let Some(r) = resolved {
                            overrides.insert(k.clone(), r);
                        }
                    }
                }
                let call = format!(
                    "__register({id}, {}, {}, {})",
                    serde_json::to_string(prop).unwrap(),
                    Value::Object(overrides),
                    serde_json::to_string(src).unwrap_or_else(|_| "\"\"".into()),
                );
                match ctx.eval(Source::from_bytes(call.as_bytes())) {
                    Ok(res) => {
                        if let Some(err) = res.as_string() {
                            let err = err.to_std_string_escaped();
                            if !err.is_empty() {
                                log::debug!("scenescript: {err}");
                                continue;
                            }
                        }
                        node_had = true;
                    }
                    Err(e) => log::debug!("scenescript register {id}/{prop}: {e}"),
                }
            }
            if node_had {
                scripted_nodes += 1;
            }
        }
        if scripted_nodes == 0 {
            return None;
        }
        log::info!("scenescript host: {scripted_nodes} scripted objects");
        Some(Self {
            ctx,
            scripted_nodes,
            disabled: false,
        })
    }

    /// Run all scripts for one frame; returns the mutated node states.
    /// `audio` is the current spectrum (0..1 bins); empty while silent.
    /// `cursor` is wallpaper UV 0..1 (WE `input.cursorPosition`).
    pub fn tick(
        &mut self,
        dt: f32,
        time_of_day: f32,
        runtime: f32,
        audio: &[f32],
        cursor: [f32; 2],
    ) -> Vec<NodeScriptState> {
        if self.disabled {
            return Vec::new();
        }
        let audio_json = if audio.is_empty() {
            "null".to_string()
        } else {
            serde_json::to_string(audio).unwrap_or_else(|_| "null".into())
        };
        // Push live cursor into the WE `input` object before scripts run.
        let cursor_js = format!(
            "input.cursorPosition.x={};input.cursorPosition.y={};input.cursorWorldPosition.x={};input.cursorWorldPosition.y={};",
            cursor[0], cursor[1], cursor[0], cursor[1]
        );
        let call = format!("__tick({dt}, {time_of_day}, {runtime}, {audio_json})");
        // Boa can panic (not return Err) on bad lexical ops. Never let that
        // take down walld — effects/particles must keep animating.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = self.ctx.eval(Source::from_bytes(cursor_js.as_bytes()));
            self.ctx.eval(Source::from_bytes(call.as_bytes()))
        }));
        match result {
            Ok(Ok(v)) => {
                let Some(s) = v.as_string() else {
                    return Vec::new();
                };
                serde_json::from_str(&s.to_std_string_escaped()).unwrap_or_default()
            }
            Ok(Err(e)) => {
                log::debug!("scenescript tick: {e}");
                Vec::new()
            }
            Err(_) => {
                log::warn!(
                    "scenescript host panicked inside Boa; disabling scripts for this scene"
                );
                self.disabled = true;
                Vec::new()
            }
        }
    }

    /// Push live decoder times into JS video handles (by layer name).
    pub fn sync_video_times(&mut self, layers: &[(String, f32, f32, bool)]) {
        for (name, time, duration, playing) in layers {
            let name_js = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".into());
            let code = format!(
                r#"(function(){{
  var L = __layersByName[{name_js}];
  if (!L) return;
  var v = L.getVideoTexture();
  v.time = {time};
  v.duration = {duration};
  // Don't clobber a script that just paused this frame; only fill if never set.
  if (v.__hostPlaying === undefined) v.__hostPlaying = true;
  v.__hostPlaying = {playing};
}})()"#,
                playing = if *playing { "true" } else { "false" },
            );
            let _ = self.ctx.eval(Source::from_bytes(code.as_bytes()));
        }
    }

    /// Read play/pause/seek requests scripts made via getVideoTexture().
    pub fn drain_video_commands(&mut self) -> Vec<VideoScriptCommand> {
        let code = r#"(function(){
  var out = [];
  for (var i = 0; i < __layerOrder.length; i++) {
    var L = __layerOrder[i];
    if (!L || !L.__video) continue;
    var v = L.__video;
    var seek = (v.__seek !== null && v.__seek !== undefined) ? v.__seek : null;
    v.__seek = null;
    out.push({
      name: L.name,
      playing: !!v.playing,
      loop: v.loop !== false,
      seek: seek
    });
  }
  return JSON.stringify(out);
})()"#;
        match self.ctx.eval(Source::from_bytes(code.as_bytes())) {
            Ok(v) => {
                let Some(s) = v.as_string() else {
                    return Vec::new();
                };
                serde_json::from_str(&s.to_std_string_escaped()).unwrap_or_default()
            }
            Err(_) => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct VideoScriptCommand {
    pub name: String,
    pub playing: bool,
    #[serde(default = "default_true")]
    pub r#loop: bool,
    pub seek: Option<f32>,
}

fn default_true() -> bool {
    true
}

fn tz_offset_minutes() -> i64 {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm.tm_gmtoff / 60
    }
}
