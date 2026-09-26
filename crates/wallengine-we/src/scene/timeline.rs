//! WE timeline animations (property `animation` objects): keyframe channels
//! played at `fps` over `length` frames, looping or one-shot. Bezier handles
//! are approximated linearly.

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Timeline {
    pub fps: f32,
    /// Total length in frames.
    pub length: f32,
    pub looped: bool,
    /// When true, keyframe values are **offsets** added to the property's
    /// static base (WE `"relative": true`). Absolute mode replaces the base.
    pub relative: bool,
    /// c0/c1/c2 keyframes as (frame, value), sorted.
    pub channels: [Vec<(f32, f32)>; 3],
}

impl Timeline {
    /// Parse from a property carrier `{ "animation": { c0.., options.. }, .. }`
    /// or a bare animation object.
    pub fn parse(v: &Value) -> Option<Self> {
        let anim = v.get("animation").unwrap_or(v);
        let obj = anim.as_object()?;
        let mut channels: [Vec<(f32, f32)>; 3] = Default::default();
        for (i, name) in ["c0", "c1", "c2"].iter().enumerate() {
            if let Some(arr) = obj.get(*name).and_then(|c| c.as_array()) {
                let mut keys: Vec<(f32, f32)> = arr
                    .iter()
                    .filter_map(|k| {
                        let f = k.get("frame")?.as_f64()? as f32;
                        let val = k.get("value").and_then(|v| {
                            v.as_f64()
                                .map(|x| x as f32)
                                .or_else(|| v.as_str()?.trim().parse().ok())
                        })?;
                        Some((f, val))
                    })
                    .collect();
                keys.sort_by(|a, b| a.0.total_cmp(&b.0));
                channels[i] = keys;
            }
        }
        if channels.iter().all(|c| c.is_empty()) {
            return None;
        }
        let opts = obj.get("options");
        let fps = opts
            .and_then(|o| o.get("fps"))
            .and_then(|v| v.as_f64())
            .unwrap_or(30.0) as f32;
        let length = opts
            .and_then(|o| o.get("length"))
            .and_then(|v| v.as_f64())
            .map(|l| l as f32)
            .unwrap_or_else(|| {
                channels
                    .iter()
                    .filter_map(|c| c.last().map(|k| k.0))
                    .fold(0.0, f32::max)
            });
        let looped = opts
            .and_then(|o| o.get("mode"))
            .and_then(|v| v.as_str())
            .map(|m| m.eq_ignore_ascii_case("loop"))
            .unwrap_or(false);
        // `"relative": true` may sit on the animation object or under options.
        let relative = anim
            .get("relative")
            .and_then(|v| v.as_bool())
            .or_else(|| opts.and_then(|o| o.get("relative")).and_then(|v| v.as_bool()))
            .or_else(|| v.get("relative").and_then(|b| b.as_bool()))
            .unwrap_or(false);
        Some(Self {
            fps,
            length: length.max(1.0),
            looped,
            relative,
            channels,
        })
    }

    /// Sample channel `chan` at `seconds` since scene start.
    pub fn sample(&self, chan: usize, seconds: f32) -> Option<f32> {
        let keys = self.channels.get(chan)?;
        if keys.is_empty() {
            return None;
        }
        let mut frame = seconds * self.fps;
        if self.looped {
            frame = frame.rem_euclid(self.length);
        } else {
            frame = frame.clamp(0.0, self.length);
        }
        if frame <= keys[0].0 {
            return Some(keys[0].1);
        }
        if frame >= keys[keys.len() - 1].0 {
            return Some(keys[keys.len() - 1].1);
        }
        for w in keys.windows(2) {
            let ((f0, v0), (f1, v1)) = (w[0], w[1]);
            if frame >= f0 && frame <= f1 {
                let t = if (f1 - f0).abs() < 1e-6 {
                    0.0
                } else {
                    (frame - f0) / (f1 - f0)
                };
                return Some(v0 + (v1 - v0) * t);
            }
        }
        Some(keys[0].1)
    }
}

/// Clock-hand style binding parsed from an angles script:
/// `engine.timeOfDay * A` or `((engine.timeOfDay * B) % 1) * A` (degrees).
#[derive(Debug, Clone, Copy)]
pub struct AngleTimeBinding {
    pub inner: f32,
    pub outer_deg: f32,
    pub frac: bool,
}

impl AngleTimeBinding {
    pub fn parse(src: &str) -> Option<Self> {
        let s: String = src.chars().filter(|c| !c.is_whitespace()).collect();
        let i = s.find("engine.timeOfDay*")?;
        let rest = &s[i + "engine.timeOfDay*".len()..];
        let (a, tail) = take_num(rest)?;
        if let Some(t2) = tail.strip_prefix(")%1)*") {
            let (b, _) = take_num(t2)?;
            Some(Self {
                inner: a,
                outer_deg: b,
                frac: true,
            })
        } else {
            Some(Self {
                inner: 1.0,
                outer_deg: a,
                frac: false,
            })
        }
    }

    /// Angle in degrees for `tod` = fraction of the day [0,1).
    pub fn angle_deg(&self, tod: f32) -> f32 {
        if self.frac {
            (tod * self.inner).fract() * self.outer_deg
        } else {
            tod * self.outer_deg
        }
    }
}

fn take_num(s: &str) -> Option<(f32, &str)> {
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    s[..end].parse().ok().map(|n| (n, &s[end..]))
}
