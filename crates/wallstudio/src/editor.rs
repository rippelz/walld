//! Scene editor — Explorer-style multi-select layers + interactive preview tools.

use crate::gizmo::{self, GizmoHandle};
use iced::keyboard::Modifiers;
use iced::mouse;
use iced::widget::image::Handle;
use iced::widget::{
    button, checkbox, column, container, image, mouse_area, responsive, row, rule, scrollable,
    slider, stack, text, text_input, Space,
};
use iced::{
    Alignment, Background, Border, Color, ContentFit, Element, Fill, Font, Length, Padding, Theme,
};
use std::collections::{BTreeSet, HashMap};
use wallengine_we::{
    spawn_preview_gif_job, EditableScene, EffectConstValue, LayerKind, LayerSummary,
    ParticleAddableKey, ParticleFieldKind, SoftPreview,
};

/// Colours come from the live palette (`theme::pal`); the sizes are layout
/// constants that never follow the wallpaper.
mod pal {
    pub use crate::theme::pal::*;
    pub const LAYERS: f32 = 300.0;
    pub const INSPECT: f32 = 340.0;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    /// Replace selection (plain click).
    Replace,
    /// Toggle membership (Ctrl/Cmd+click).
    Toggle,
    /// Range from last anchor (Shift+click).
    Range,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Move,
    Scale,
    Rotate,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select (V)",
            Self::Move => "Move (W)",
            Self::Scale => "Scale (E)",
            Self::Rotate => "Rotate (R)",
        }
    }
}

#[derive(Debug, Clone)]
struct DragState {
    tool: Tool,
    handle: GizmoHandle,
    /// Ortho-space cursor at press.
    start_ortho: [f32; 2],
    last_ortho: [f32; 2],
    origins: Vec<(usize, [f32; 3])>,
    scales: Vec<(usize, [f32; 3])>,
    angles_deg: Vec<(usize, [f32; 3])>,
    /// Pivot = average origin of selection at drag start.
    pivot: [f32; 2],
}

#[derive(Debug, Clone)]
pub enum EditorMessage {
    /// Left-click select (uses Ctrl/Shift modifiers from host).
    SelectLayer(usize),
    /// Always toggle membership (right-click / explicit multi).
    ToggleLayer(usize),
    /// Always range-select from anchor.
    RangeSelectLayer(usize),
    ToggleVisible(usize, bool),
    SetOriginX(f32),
    SetOriginY(f32),
    SetOriginZ(f32),
    SetScaleX(f32),
    SetScaleY(f32),
    SetScaleZ(f32),
    SetAngleX(f32),
    SetAngleY(f32),
    SetAngleZ(f32),
    SetAlpha(f32),
    SetBrightness(f32),
    /// Layer tint RGB component 0..2.
    SetLayerColor(usize, f32),
    SetParticleRate(f32),
    SetParticleSpeed(f32),
    SetParticleSize(f32),
    SetParticleAlpha(f32),
    SetParticleCount(f32),
    SetParticleLifetime(f32),
    SetParticleColorN(usize, f32),
    /// Particle system JSON float (`pd:…` key from ParticleField).
    ParticleDocFloat(String, f32),
    /// Commit typed particle text field (material, name, animationmode, …).
    ParticleDocTextCommit(String),
    /// Add a known missing particle JSON key (`pd:…` + default encoded as string).
    ParticleAddKey(String, String),
    /// Freeform add: slot (`op1`, `em0`, `sys`) uses draft `addkey:{slot}`.
    ParticleAddCustom(String),
    EffectVisible(usize, bool),
    EffectConst(usize, usize, String, f32),
    /// Vec effect constant component.
    EffectConstComp(usize, usize, String, usize, f32),
    /// Typed number field draft (key like "ox", "fx:0:0:speed").
    NumDraft(String, String),
    /// Commit typed number (Enter). Value may exceed slider bounds.
    NumCommit(String),
    Rename(String),
    CommitRename,
    TextDraft(String),
    CommitText,
    /// Assign package-relative image/model path to the primary image layer.
    AssignImage(String),
    Duplicate,
    Delete,
    /// Draw order: lower index = behind, higher = in front.
    MoveBack,
    MoveFront,
    Undo,
    Redo,
    Save,
    RefreshPreview,
    TogglePlay,
    OpenFolder,
    ImportImagePath(String),
    CommitImport,
    RefreshAssets,
    SetTool(Tool),
    /// Gizmo handle grab (screen coords in preview widget).
    GizmoPress {
        handle: GizmoHandle,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    GizmoDrag {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    GizmoRelease,
    /// Click empty canvas / select tool pick.
    CanvasPressAt {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    ClearSelection,
    SelectAll,
    AnimTick,
    Close,
}

pub struct EditorSession {
    pub scene: EditableScene,
    /// Ordered selection (BTreeSet keeps stable order by index).
    pub selected: BTreeSet<usize>,
    /// Anchor for shift-range select.
    pub select_anchor: Option<usize>,
    /// Last clicked layer — inspector primary (not lowest index).
    pub primary_index: Option<usize>,
    pub rename_draft: String,
    pub text_draft: String,
    pub import_path: String,
    pub status: String,
    pub status_ok: bool,
    pub assets: Vec<String>,
    pub preview: Option<SoftPreview>,
    pub preview_handle: Option<Handle>,
    pub preview_gen: u64,
    pub playing: bool,
    reload_pending: bool,
    frames_since_edit: u32,
    pub tool: Tool,
    drag: Option<DragState>,
    /// Latest keyboard modifiers (set by host before update).
    pub input_mods: Modifiers,
    /// Cursor ortho pos for status (debug/help).
    cursor_ortho: Option<[f32; 2]>,
    /// Text drafts for numeric inspector fields (allows out-of-slider values).
    pub num_drafts: HashMap<String, String>,
    /// Continuous slider gesture id — one undo snapshot per drag of the same control.
    slider_gesture: Option<String>,
    /// Debounce counter for auto preview.gif regeneration after edits.
    gif_regen_pending: bool,
    gif_idle_ticks: u32,
    /// True after a gif job was kicked (status line).
    gif_busy: bool,
}

impl EditorSession {
    pub fn open(scene: EditableScene) -> Self {
        let layers = scene.layers();
        let mut selected = BTreeSet::new();
        if let Some(l) = layers.first() {
            selected.insert(l.index);
        }
        let rename_draft = layers.first().map(|l| l.name.clone()).unwrap_or_default();
        let assets = scene.list_assets(400);
        let dir = scene.dir.clone();
        let id = scene.id.clone();
        let title = scene.title.clone();
        let anchor = selected.iter().next().copied();

        let mut s = Self {
            scene,
            selected,
            select_anchor: anchor,
            primary_index: anchor,
            rename_draft,
            text_draft: String::new(),
            import_path: String::new(),
            status: String::new(),
            status_ok: true,
            assets,
            preview: None,
            preview_handle: None,
            preview_gen: 0,
            playing: true,
            reload_pending: false,
            frames_since_edit: 0,
            tool: Tool::Move,
            drag: None,
            input_mods: Modifiers::default(),
            cursor_ortho: None,
            num_drafts: HashMap::new(),
            slider_gesture: None,
            gif_regen_pending: false,
            gif_idle_ticks: 0,
            gif_busy: false,
        };
        if let Some(i) = s.primary() {
            let _ = s.scene.ensure_particle_doc(i);
            s.text_draft = s.scene.read_text_literal(i).unwrap_or_default();
        }
        s.sync_num_drafts();

        match SoftPreview::start(&dir, &id, &title) {
            Ok(p) => {
                if let Some(f) = p.current_frame() {
                    s.preview_gen = f.generation;
                    s.preview_handle = Some(Handle::from_rgba(f.width, f.height, f.rgba));
                }
                s.preview = Some(p);
                s.ok("GPU preview · W move · E scale · R rotate · V select");
                // Fresh fork / open: bake an animated thumbnail once the first
                // frames are on disk.
                s.schedule_preview_gif();
            }
            Err(e) => s.fail(&format!("preview: {e}")),
        }
        s
    }

    fn schedule_preview_gif(&mut self) {
        self.gif_regen_pending = true;
        self.gif_idle_ticks = 0;
    }

    fn maybe_kick_preview_gif(&mut self) {
        if !self.gif_regen_pending || self.drag.is_some() {
            return;
        }
        // ~2s idle after last edit (AnimTick is ~80ms).
        self.gif_idle_ticks = self.gif_idle_ticks.saturating_add(1);
        if self.gif_idle_ticks < 25 {
            return;
        }
        self.gif_regen_pending = false;
        self.gif_idle_ticks = 0;
        // Ensure latest scene is on disk before capture.
        if self.scene.dirty {
            if let Err(e) = self.scene.save() {
                self.fail(&e);
                return;
            }
        }
        let dir = self.scene.dir.clone();
        self.gif_busy = true;
        self.ok("updating library thumbnail (preview.gif)…");
        spawn_preview_gif_job(dir);
    }

    pub fn title(&self) -> String {
        let dirty = if self.scene.dirty { " ●" } else { "" };
        format!("wallstudio editor — {}{dirty}", self.scene.title)
    }

    pub fn primary(&self) -> Option<usize> {
        if let Some(p) = self.primary_index {
            if self.selected.contains(&p) {
                return Some(p);
            }
        }
        self.selected.iter().next().copied()
    }

    fn select_mode(&self) -> SelectMode {
        if self.input_mods.command() || self.input_mods.control() {
            SelectMode::Toggle
        } else if self.input_mods.shift() {
            SelectMode::Range
        } else {
            SelectMode::Replace
        }
    }

    fn end_slider_gesture(&mut self) {
        self.slider_gesture = None;
    }

    /// One undo snapshot for a continuous drag of the same control id.
    fn begin_slider_gesture(&mut self, id: &str) {
        if self.slider_gesture.as_deref() != Some(id) {
            self.scene.snapshot_undo();
            self.slider_gesture = Some(id.to_string());
        }
    }

    fn apply_selection(&mut self, index: usize, mode: SelectMode) {
        let n = self.scene.layers().len();
        if index >= n {
            return;
        }
        self.end_slider_gesture();
        match mode {
            SelectMode::Replace => {
                self.selected.clear();
                self.selected.insert(index);
                self.select_anchor = Some(index);
                self.primary_index = Some(index);
            }
            SelectMode::Toggle => {
                if self.selected.contains(&index) {
                    self.selected.remove(&index);
                    if self.primary_index == Some(index) {
                        self.primary_index = self.selected.iter().next().copied();
                    }
                } else {
                    self.selected.insert(index);
                    self.primary_index = Some(index);
                }
                self.select_anchor = Some(index);
            }
            SelectMode::Range => {
                let anchor = self.select_anchor.unwrap_or(index);
                let (a, b) = if anchor <= index {
                    (anchor, index)
                } else {
                    (index, anchor)
                };
                self.selected.clear();
                for i in a..=b.min(n.saturating_sub(1)) {
                    self.selected.insert(i);
                }
                // Range end is the primary (last clicked).
                self.primary_index = Some(index);
            }
        }
        if let Some(i) = self.primary() {
            if let Some(l) = self.scene.layers().into_iter().find(|l| l.index == i) {
                self.rename_draft = l.name;
            }
            self.text_draft = self.scene.read_text_literal(i).unwrap_or_default();
            if let Some(m) = self.scene.read_image_model(i) {
                self.num_drafts.insert("imgpath".into(), m);
            }
            // Warm particle JSON cache so the inspector can list every field.
            let _ = self.scene.ensure_particle_doc(i);
        }
        self.sync_num_drafts();
    }

    /// Refresh text boxes from the primary selection (doesn't clamp).
    fn sync_num_drafts(&mut self) {
        let Some(i) = self.primary() else {
            self.num_drafts.clear();
            return;
        };
        let o = self.scene.read_origin(i);
        let s = self.scene.read_scale(i);
        let a = self.scene.read_angles_degrees(i);
        let alpha = self.scene.read_alpha(i);
        let br = self.scene.read_brightness(i);
        let fmt = |v: f32| {
            if (v - v.round()).abs() < 1e-4 && v.abs() < 1e7 {
                format!("{}", v.round() as i64)
            } else {
                format!("{v:.4}")
            }
        };
        self.num_drafts.insert("ox".into(), fmt(o[0]));
        self.num_drafts.insert("oy".into(), fmt(o[1]));
        self.num_drafts.insert("oz".into(), fmt(o[2]));
        self.num_drafts.insert("sx".into(), fmt(s[0]));
        self.num_drafts.insert("sy".into(), fmt(s[1]));
        self.num_drafts.insert("sz".into(), fmt(s[2]));
        self.num_drafts.insert("ax".into(), fmt(a[0]));
        self.num_drafts.insert("ay".into(), fmt(a[1]));
        self.num_drafts.insert("az".into(), fmt(a[2]));
        self.num_drafts.insert("alpha".into(), fmt(alpha));
        self.num_drafts.insert("bright".into(), fmt(br));
        let layer_col = self.scene.read_color(i).unwrap_or([1.0, 1.0, 1.0]);
        self.num_drafts.insert("lcolr".into(), fmt(layer_col[0]));
        self.num_drafts.insert("lcolg".into(), fmt(layer_col[1]));
        self.num_drafts.insert("lcolb".into(), fmt(layer_col[2]));
        for key in ["rate", "speed", "size", "palpha", "count", "lifetime"] {
            let scene_key = if key == "palpha" { "alpha" } else { key };
            let v = self.scene.read_particle_override_f32(i, scene_key);
            self.num_drafts.insert(key.into(), fmt(v));
        }
        let col = self.scene.read_particle_override_colorn(i);
        self.num_drafts.insert("pcolr".into(), fmt(col[0]));
        self.num_drafts.insert("pcolg".into(), fmt(col[1]));
        self.num_drafts.insert("pcolb".into(), fmt(col[2]));
        // Full particle system JSON fields
        for field in self.scene.particle_fields(i) {
            match &field.kind {
                ParticleFieldKind::Float { value, .. } => {
                    self.num_drafts.insert(field.key.clone(), fmt(*value));
                }
                ParticleFieldKind::Text { value } => {
                    self.num_drafts.insert(field.key.clone(), value.clone());
                }
            }
        }
        // Effect constants (all types)
        for ef in self.scene.effects_on(i) {
            for c in &ef.constants {
                match &c.value {
                    EffectConstValue::Float(f) => {
                        let k = format!("fx:{}:{}:{}", ef.index, c.pass, c.key);
                        self.num_drafts.insert(k, fmt(*f));
                    }
                    EffectConstValue::Vec2(v) => {
                        for (axis, val) in v.iter().enumerate() {
                            let k = format!("fx:{}:{}:{}:{}", ef.index, c.pass, c.key, axis);
                            self.num_drafts.insert(k, fmt(*val));
                        }
                    }
                    EffectConstValue::Vec3(v) => {
                        for (axis, val) in v.iter().enumerate() {
                            let k = format!("fx:{}:{}:{}:{}", ef.index, c.pass, c.key, axis);
                            self.num_drafts.insert(k, fmt(*val));
                        }
                    }
                    EffectConstValue::Vec4(v) => {
                        for (axis, val) in v.iter().enumerate() {
                            let k = format!("fx:{}:{}:{}:{}", ef.index, c.pass, c.key, axis);
                            self.num_drafts.insert(k, fmt(*val));
                        }
                    }
                    EffectConstValue::Text(s) => {
                        let k = format!("fx:{}:{}:{}", ef.index, c.pass, c.key);
                        self.num_drafts.insert(k, s.clone());
                    }
                    EffectConstValue::Other => {}
                }
            }
        }
    }

    fn draft(&self, key: &str) -> String {
        self.num_drafts
            .get(key)
            .cloned()
            .unwrap_or_else(|| "0".into())
    }

    fn set_draft(&mut self, key: &str, text: String) {
        self.num_drafts.insert(key.to_string(), text);
    }

    fn commit_num(&mut self, key: &str) {
        self.end_slider_gesture();
        let Some(raw) = self.num_drafts.get(key).cloned() else {
            return;
        };
        // Image path assignment (text).
        if key == "imgpath" {
            if let Some(i) = self.primary() {
                self.apply_edit(|s| s.set_image_model(i, raw.trim()));
            }
            self.sync_num_drafts();
            return;
        }
        // Particle text fields (material, name, animationmode, type, …).
        if key.starts_with("pd:") {
            if let Ok(v) = raw.trim().parse::<f32>() {
                if v.is_finite() {
                    if let Some(i) = self.primary() {
                        self.apply_edit(|s| s.set_particle_field_f32(i, key, v));
                    }
                    self.sync_num_drafts();
                    return;
                }
            }
            // Non-numeric → text write.
            if let Some(i) = self.primary() {
                self.apply_edit(|s| s.set_particle_field_text(i, key, raw.trim()));
            }
            self.sync_num_drafts();
            return;
        }
        // Effect text constants: fx:{ei}:{pass}:{key} without axis.
        if key.starts_with("fx:") && raw.trim().parse::<f32>().is_err() {
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() == 4 {
                if let (Ok(ei), Ok(pass)) = (parts[1].parse::<usize>(), parts[2].parse::<usize>()) {
                    if let Some(i) = self.primary() {
                        let fkey = parts[3].to_string();
                        self.scene.snapshot_undo();
                        let _ =
                            self.scene
                                .set_effect_constant_text_raw(i, ei, pass, &fkey, raw.trim());
                        self.after_edit();
                    }
                }
            }
            self.sync_num_drafts();
            return;
        }
        let Ok(v) = raw.trim().parse::<f32>() else {
            self.fail(&format!("invalid number: {raw}"));
            self.sync_num_drafts();
            return;
        };
        if !v.is_finite() {
            self.fail("number must be finite");
            self.sync_num_drafts();
            return;
        }
        // Typed values may exceed slider track limits (user request).
        match key {
            "ox" => self.map_selected_origin(0, v),
            "oy" => self.map_selected_origin(1, v),
            "oz" => self.map_selected_origin(2, v),
            "sx" => self.map_selected_scale(0, v),
            "sy" => self.map_selected_scale(1, v),
            "sz" => self.map_selected_scale(2, v),
            "ax" => self.map_selected_angle(0, v),
            "ay" => self.map_selected_angle(1, v),
            "az" => self.map_selected_angle(2, v),
            "alpha" => self.map_selected_alpha(v),
            "bright" => {
                if let Some(i) = self.primary() {
                    self.end_slider_gesture();
                    self.scene.snapshot_undo();
                    let _ = self.scene.set_brightness_raw(i, v.max(0.0));
                    self.after_edit();
                }
            }
            "lcolr" => self.map_layer_color(0, v),
            "lcolg" => self.map_layer_color(1, v),
            "lcolb" => self.map_layer_color(2, v),
            "rate" => self.map_particle("rate", v),
            "speed" => self.map_particle("speed", v),
            "size" => self.map_particle("size", v),
            "palpha" => self.map_particle("alpha", v),
            "count" => self.map_particle("count", v),
            "lifetime" => self.map_particle("lifetime", v),
            "pcolr" => self.map_particle_color(0, v),
            "pcolg" => self.map_particle_color(1, v),
            "pcolb" => self.map_particle_color(2, v),
            other if other.starts_with("fx:") => {
                // fx:{ei}:{pass}:{key} or fx:{ei}:{pass}:{key}:{axis}
                let parts: Vec<&str> = other.split(':').collect();
                if parts.len() >= 4 {
                    if let (Ok(ei), Ok(pass)) =
                        (parts[1].parse::<usize>(), parts[2].parse::<usize>())
                    {
                        if let Some(i) = self.primary() {
                            if parts.len() >= 5 {
                                if let Ok(axis) = parts[4].parse::<usize>() {
                                    let fkey = parts[3].to_string();
                                    self.end_slider_gesture();
                                    self.scene.snapshot_undo();
                                    let _ = self
                                        .scene
                                        .set_effect_constant_comp_raw(i, ei, pass, &fkey, axis, v);
                                    self.after_edit();
                                }
                            } else {
                                let fkey = parts[3].to_string();
                                // Text vs float: if raw wasn't a number we'd have failed earlier.
                                self.end_slider_gesture();
                                self.scene.snapshot_undo();
                                let _ = self
                                    .scene
                                    .set_effect_constant_f32_raw(i, ei, pass, &fkey, v);
                                self.after_edit();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        self.sync_num_drafts();
    }

    fn poll_preview_frame(&mut self) {
        if let Some(p) = self.preview.as_mut() {
            if let Some(f) = p.take_if_new() {
                self.preview_gen = f.generation;
                self.preview_handle = Some(Handle::from_rgba(f.width, f.height, f.rgba));
            }
            if let Some(err) = p.last_error() {
                self.fail(&err);
            }
        }
    }

    fn mark_reload(&mut self) {
        self.reload_pending = true;
        self.frames_since_edit = 0;
    }

    fn do_reload_preview(&mut self) {
        if self.scene.dirty {
            if let Err(e) = self.scene.save() {
                self.fail(&e);
                return;
            }
        }
        if let Some(p) = &self.preview {
            p.request_reload();
            self.ok("preview reloading…");
        }
        self.reload_pending = false;
    }

    /// Map widget pixel → WE ortho (bottom-left origin, Y-up), ContentFit::Contain.
    fn widget_to_ortho(&self, x: f32, y: f32, widget_w: f32, widget_h: f32) -> Option<[f32; 2]> {
        let (ow, oh) = self.scene.ortho();
        if widget_w < 1.0 || widget_h < 1.0 || ow < 1.0 || oh < 1.0 {
            return None;
        }
        let s = (widget_w / ow).min(widget_h / oh);
        let drawn_w = ow * s;
        let drawn_h = oh * s;
        let ox = (widget_w - drawn_w) * 0.5;
        let oy = (widget_h - drawn_h) * 0.5;
        if x < ox || y < oy || x > ox + drawn_w || y > oy + drawn_h {
            return None;
        }
        let u = (x - ox) / drawn_w;
        let v = (y - oy) / drawn_h;
        Some([u * ow, (1.0 - v) * oh])
    }

    fn selection_pivot(&self) -> [f32; 2] {
        if self.selected.is_empty() {
            let (ow, oh) = self.scene.ortho();
            return [ow * 0.5, oh * 0.5];
        }
        let mut sx = 0.0f32;
        let mut sy = 0.0f32;
        let mut n = 0.0f32;
        for &i in &self.selected {
            let o = self.scene.read_origin(i);
            sx += o[0];
            sy += o[1];
            n += 1.0;
        }
        [sx / n, sy / n]
    }

    fn handle_canvas_press_at(&mut self, ortho: [f32; 2]) {
        // Select tool or empty-area click: pick layer by proximity.
        if let Some(hit) = self.pick_layer_at(ortho) {
            self.apply_selection(hit, self.select_mode());
            self.ok(&format!("picked layer {}", hit));
        } else if !self.input_mods.shift()
            && !self.input_mods.control()
            && !self.input_mods.command()
        {
            self.selected.clear();
        }
    }

    fn begin_gizmo_drag(&mut self, handle: GizmoHandle, ortho: [f32; 2]) {
        if self.selected.is_empty() {
            return;
        }
        self.scene.snapshot_undo();
        let mut origins = Vec::new();
        let mut scales = Vec::new();
        let mut angles_deg = Vec::new();
        for &i in &self.selected {
            origins.push((i, self.scene.read_origin(i)));
            scales.push((i, self.scene.read_scale(i)));
            angles_deg.push((i, self.scene.read_angles_degrees(i)));
        }
        let pivot = self.selection_pivot();
        self.drag = Some(DragState {
            tool: self.tool,
            handle,
            start_ortho: ortho,
            last_ortho: ortho,
            origins,
            scales,
            angles_deg,
            pivot,
        });
        self.reload_pending = false;
        self.ok(&format!(
            "dragging {:?} · {} layers",
            handle,
            self.selected.len()
        ));
    }

    fn apply_gizmo_drag(&mut self, ortho: [f32; 2]) {
        let Some(drag) = self.drag.clone() else {
            return;
        };
        let dx = ortho[0] - drag.start_ortho[0];
        let dy = ortho[1] - drag.start_ortho[1];
        match drag.handle {
            GizmoHandle::MoveFree => {
                for (i, o) in &drag.origins {
                    let _ = self.scene.set_origin_raw(*i, [o[0] + dx, o[1] + dy, o[2]]);
                }
            }
            GizmoHandle::MoveX => {
                for (i, o) in &drag.origins {
                    let _ = self.scene.set_origin_raw(*i, [o[0] + dx, o[1], o[2]]);
                }
            }
            GizmoHandle::MoveY => {
                for (i, o) in &drag.origins {
                    let _ = self.scene.set_origin_raw(*i, [o[0], o[1] + dy, o[2]]);
                }
            }
            GizmoHandle::ScaleUniform => {
                let v0 = [
                    drag.start_ortho[0] - drag.pivot[0],
                    drag.start_ortho[1] - drag.pivot[1],
                ];
                let v1 = [ortho[0] - drag.pivot[0], ortho[1] - drag.pivot[1]];
                let d0 = (v0[0] * v0[0] + v0[1] * v0[1]).sqrt().max(8.0);
                let d1 = (v1[0] * v1[0] + v1[1] * v1[1]).sqrt().max(1.0);
                let factor = (d1 / d0).clamp(0.05, 20.0);
                for (i, s) in &drag.scales {
                    let _ = self.scene.set_scale_raw(
                        *i,
                        [
                            (s[0] * factor).clamp(0.01, 50.0),
                            (s[1] * factor).clamp(0.01, 50.0),
                            s[2],
                        ],
                    );
                }
            }
            GizmoHandle::ScaleX => {
                let d0 = (drag.start_ortho[0] - drag.pivot[0]).abs().max(8.0);
                let d1 = (ortho[0] - drag.pivot[0]).abs().max(1.0);
                let factor = (d1 / d0).clamp(0.05, 20.0);
                for (i, s) in &drag.scales {
                    let _ = self
                        .scene
                        .set_scale_raw(*i, [(s[0] * factor).clamp(0.01, 50.0), s[1], s[2]]);
                }
            }
            GizmoHandle::ScaleY => {
                let d0 = (drag.start_ortho[1] - drag.pivot[1]).abs().max(8.0);
                let d1 = (ortho[1] - drag.pivot[1]).abs().max(1.0);
                let factor = (d1 / d0).clamp(0.05, 20.0);
                for (i, s) in &drag.scales {
                    let _ = self
                        .scene
                        .set_scale_raw(*i, [s[0], (s[1] * factor).clamp(0.01, 50.0), s[2]]);
                }
            }
            GizmoHandle::Rotate => {
                let a0 = (drag.start_ortho[1] - drag.pivot[1])
                    .atan2(drag.start_ortho[0] - drag.pivot[0]);
                let a1 = (ortho[1] - drag.pivot[1]).atan2(ortho[0] - drag.pivot[0]);
                let ddeg = (a1 - a0).to_degrees();
                for (i, ang) in &drag.angles_deg {
                    let _ = self
                        .scene
                        .set_angles_degrees_raw(*i, [ang[0], ang[1], ang[2] + ddeg]);
                }
            }
        }
        if let Some(d) = self.drag.as_mut() {
            d.last_ortho = ortho;
        }
        self.status_ok = true;
    }

    fn end_gizmo_drag(&mut self) {
        if self.drag.take().is_some() {
            self.after_edit();
            self.ok(&format!(
                "{} · {} layer(s) updated",
                self.tool.label(),
                self.selected.len()
            ));
        }
    }

    /// Hit-test: pick topmost layer whose origin is near ortho point (rough).
    fn pick_layer_at(&self, ortho: [f32; 2]) -> Option<usize> {
        let layers = self.scene.layers();
        let mut best: Option<(usize, f32)> = None;
        for l in layers.iter().rev() {
            // Skip invisible for pick
            if !l.visible {
                continue;
            }
            let o = self.scene.read_origin(l.index);
            let sc = self.scene.read_scale(l.index);
            // Approximate radius from scale * 64px default (unknown size for groups).
            let r = 80.0 * sc[0].abs().max(sc[1].abs()).max(0.25);
            let dx = o[0] - ortho[0];
            let dy = o[1] - ortho[1];
            let d2 = dx * dx + dy * dy;
            if d2 <= r * r {
                let score = d2;
                if best.map(|(_, b)| score < b).unwrap_or(true) {
                    best = Some((l.index, score));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    pub fn update(&mut self, msg: EditorMessage) -> bool {
        match msg {
            EditorMessage::Close => return true,
            EditorMessage::AnimTick => {
                if self.drag.is_none() && self.reload_pending {
                    self.frames_since_edit = self.frames_since_edit.saturating_add(1);
                    if self.frames_since_edit >= 2 {
                        self.do_reload_preview();
                    }
                }
                self.poll_preview_frame();
                self.maybe_kick_preview_gif();
            }
            EditorMessage::SelectLayer(i) => {
                let mode = self.select_mode();
                self.apply_selection(i, mode);
                self.ok(&format!(
                    "{} selected · {}",
                    self.selected.len(),
                    match mode {
                        SelectMode::Replace => "click",
                        SelectMode::Toggle => "ctrl multi",
                        SelectMode::Range => "shift range",
                    }
                ));
            }
            EditorMessage::ToggleLayer(i) => {
                self.apply_selection(i, SelectMode::Toggle);
                self.ok(&format!("{} selected (toggle)", self.selected.len()));
            }
            EditorMessage::RangeSelectLayer(i) => {
                self.apply_selection(i, SelectMode::Range);
                self.ok(&format!("{} selected (range)", self.selected.len()));
            }
            EditorMessage::ClearSelection => {
                self.end_slider_gesture();
                self.selected.clear();
                self.select_anchor = None;
                self.primary_index = None;
            }
            EditorMessage::SelectAll => {
                self.end_slider_gesture();
                self.selected = self.scene.layers().into_iter().map(|l| l.index).collect();
                self.select_anchor = self.primary();
                if self.primary_index.is_none() {
                    self.primary_index = self.selected.iter().next().copied();
                }
            }
            EditorMessage::SetTool(t) => {
                self.end_slider_gesture();
                self.tool = t;
                self.ok(t.label());
            }
            EditorMessage::GizmoPress { handle, x, y, w, h } => {
                self.end_slider_gesture();
                if let Some(ortho) = self.widget_to_ortho(x, y, w, h) {
                    self.cursor_ortho = Some(ortho);
                    self.begin_gizmo_drag(handle, ortho);
                }
            }
            EditorMessage::GizmoDrag { x, y, w, h } => {
                if let Some(ortho) = self.widget_to_ortho(x, y, w, h) {
                    self.cursor_ortho = Some(ortho);
                    self.apply_gizmo_drag(ortho);
                }
            }
            EditorMessage::GizmoRelease => {
                self.end_gizmo_drag();
            }
            EditorMessage::CanvasPressAt { x, y, w, h } => {
                if let Some(ortho) = self.widget_to_ortho(x, y, w, h) {
                    self.cursor_ortho = Some(ortho);
                    self.handle_canvas_press_at(ortho);
                }
            }
            EditorMessage::ToggleVisible(i, v) => {
                if let Err(e) = self.scene.set_visible(i, v) {
                    self.fail(&e);
                } else {
                    // Apply visibility immediately (don't wait on debounce).
                    if let Err(e) = self.scene.save() {
                        self.fail(&e);
                    } else {
                        self.scene.dirty = false;
                        if let Some(p) = &self.preview {
                            p.request_reload();
                        }
                        let name = self
                            .scene
                            .layers()
                            .into_iter()
                            .find(|l| l.index == i)
                            .map(|l| l.name)
                            .unwrap_or_else(|| format!("#{i}"));
                        self.ok(&format!(
                            "{} «{}» — preview reloading",
                            if v { "shown" } else { "hidden" },
                            name
                        ));
                        self.schedule_preview_gif();
                    }
                }
            }
            EditorMessage::SetOriginX(x) => self.map_selected_origin(0, x),
            EditorMessage::SetOriginY(y) => self.map_selected_origin(1, y),
            EditorMessage::SetOriginZ(z) => self.map_selected_origin(2, z),
            EditorMessage::SetScaleX(x) => self.map_selected_scale(0, x),
            EditorMessage::SetScaleY(y) => self.map_selected_scale(1, y),
            EditorMessage::SetScaleZ(z) => self.map_selected_scale(2, z),
            EditorMessage::SetAngleX(deg) => self.map_selected_angle(0, deg),
            EditorMessage::SetAngleY(deg) => self.map_selected_angle(1, deg),
            EditorMessage::SetAngleZ(deg) => self.map_selected_angle(2, deg),
            EditorMessage::SetAlpha(a) => self.map_selected_alpha(a),
            EditorMessage::SetBrightness(b) => {
                if let Some(i) = self.primary() {
                    self.begin_slider_gesture("bright");
                    let _ = self.scene.set_brightness_raw(i, b);
                    self.after_edit();
                }
            }
            EditorMessage::SetLayerColor(axis, v) => self.map_layer_color(axis, v),
            EditorMessage::SetParticleRate(v) => self.map_particle("rate", v),
            EditorMessage::SetParticleSpeed(v) => self.map_particle("speed", v),
            EditorMessage::SetParticleSize(v) => self.map_particle("size", v),
            EditorMessage::SetParticleAlpha(v) => self.map_particle("alpha", v),
            EditorMessage::SetParticleCount(v) => self.map_particle("count", v),
            EditorMessage::SetParticleLifetime(v) => self.map_particle("lifetime", v),
            EditorMessage::SetParticleColorN(axis, v) => self.map_particle_color(axis, v),
            EditorMessage::ParticleDocFloat(key, v) => {
                if let Some(i) = self.primary() {
                    self.begin_slider_gesture(&format!("pd:{key}"));
                    let _ = self.scene.set_particle_field_f32_raw(i, &key, v);
                    self.after_edit();
                }
            }
            EditorMessage::ParticleDocTextCommit(key) => {
                self.end_slider_gesture();
                let raw = self.num_drafts.get(&key).cloned().unwrap_or_default();
                if let Some(i) = self.primary() {
                    self.apply_edit(|s| s.set_particle_field_text(i, &key, raw.trim()));
                }
            }
            EditorMessage::ParticleAddKey(key, def_json) => {
                self.end_slider_gesture();
                if let Some(i) = self.primary() {
                    let default = serde_json::from_str(&def_json).unwrap_or(serde_json::json!(0.0));
                    match self.scene.add_particle_key(i, &key, default) {
                        Ok(()) => {
                            self.after_edit();
                            self.ok(&format!("added {key}"));
                        }
                        Err(e) => self.fail(&e),
                    }
                }
            }
            EditorMessage::ParticleAddCustom(slot) => {
                self.end_slider_gesture();
                let draft_key = format!("addkey:{slot}");
                let field = self.num_drafts.get(&draft_key).cloned().unwrap_or_default();
                if let Some(i) = self.primary() {
                    match self.scene.add_particle_key_freeform(i, &slot, &field) {
                        Ok(()) => {
                            self.num_drafts.insert(draft_key, String::new());
                            self.after_edit();
                            self.ok(&format!("added {field}"));
                        }
                        Err(e) => self.fail(&e),
                    }
                }
            }
            EditorMessage::EffectVisible(ei, v) => {
                self.end_slider_gesture();
                if let Some(i) = self.primary() {
                    if let Err(e) = self.scene.set_effect_visible(i, ei, v) {
                        self.fail(&e);
                    } else if let Err(e) = self.scene.save() {
                        self.fail(&e);
                    } else {
                        self.scene.dirty = false;
                        if let Some(p) = &self.preview {
                            p.request_reload();
                        }
                        self.ok(&format!("effect {} {}", ei, if v { "on" } else { "off" }));
                        self.schedule_preview_gif();
                    }
                }
            }
            EditorMessage::EffectConst(ei, pass, key, v) => {
                if let Some(i) = self.primary() {
                    self.begin_slider_gesture(&format!("fx:{ei}:{pass}:{key}"));
                    let _ = self.scene.set_effect_constant_f32_raw(i, ei, pass, &key, v);
                    self.after_edit();
                }
            }
            EditorMessage::EffectConstComp(ei, pass, key, axis, v) => {
                if let Some(i) = self.primary() {
                    self.begin_slider_gesture(&format!("fx:{ei}:{pass}:{key}:{axis}"));
                    let _ = self
                        .scene
                        .set_effect_constant_comp_raw(i, ei, pass, &key, axis, v);
                    self.after_edit();
                }
            }
            EditorMessage::NumDraft(key, text) => self.set_draft(&key, text),
            EditorMessage::NumCommit(key) => self.commit_num(&key),
            EditorMessage::Rename(s) => self.rename_draft = s,
            EditorMessage::CommitRename => {
                self.end_slider_gesture();
                if let Some(i) = self.primary() {
                    let name = self.rename_draft.clone();
                    self.apply_edit(|s| s.set_name(i, &name));
                }
            }
            EditorMessage::TextDraft(s) => self.text_draft = s,
            EditorMessage::CommitText => {
                self.end_slider_gesture();
                if let Some(i) = self.primary() {
                    let t = self.text_draft.clone();
                    self.apply_edit(|s| s.set_text_literal(i, &t));
                }
            }
            EditorMessage::AssignImage(path) => {
                self.end_slider_gesture();
                if let Some(i) = self.primary() {
                    match self.scene.set_image_model(i, &path) {
                        Ok(()) => {
                            self.after_edit();
                            self.ok(&format!("image → {path}"));
                        }
                        Err(e) => self.fail(&e),
                    }
                } else {
                    self.fail("select an image layer first");
                }
            }
            EditorMessage::Duplicate => {
                self.end_slider_gesture();
                let indices: Vec<usize> = self.selected.iter().copied().collect();
                if indices.is_empty() {
                    return false;
                }
                self.scene.snapshot_undo();
                let mut new_sel = BTreeSet::new();
                let mut sorted = indices;
                sorted.sort_unstable();
                for i in sorted.into_iter().rev() {
                    match self.scene.duplicate_layer_raw(i) {
                        Ok(ni) => {
                            new_sel.insert(ni);
                        }
                        Err(e) => {
                            self.fail(&e);
                            return false;
                        }
                    }
                }
                self.selected = new_sel;
                self.primary_index = self.selected.iter().next().copied();
                self.after_edit();
                self.ok("duplicated selection");
            }
            EditorMessage::Delete => {
                self.end_slider_gesture();
                let mut indices: Vec<usize> = self.selected.iter().copied().collect();
                if indices.is_empty() {
                    return false;
                }
                indices.sort_unstable();
                self.scene.snapshot_undo();
                for i in indices.into_iter().rev() {
                    if let Err(e) = self.scene.delete_layer_raw(i) {
                        self.fail(&e);
                        return false;
                    }
                }
                self.selected.clear();
                self.select_anchor = None;
                self.primary_index = None;
                self.after_edit();
                self.ok("deleted selection");
            }
            // Z-order: lower list index draws first (behind); higher = in front.
            EditorMessage::MoveBack => {
                self.end_slider_gesture();
                let mut indices: Vec<usize> = self.selected.iter().copied().collect();
                indices.sort_unstable();
                if indices.is_empty() || indices[0] == 0 {
                    return false;
                }
                self.scene.snapshot_undo();
                let mut new_sel = BTreeSet::new();
                let mut new_primary = self.primary_index;
                for i in indices {
                    if let Err(e) = self.scene.move_layer_raw(i, i - 1) {
                        self.fail(&e);
                        return false;
                    }
                    new_sel.insert(i - 1);
                    if self.primary_index == Some(i) {
                        new_primary = Some(i - 1);
                    }
                }
                self.selected = new_sel;
                self.primary_index = new_primary;
                self.after_edit();
                self.ok("moved back (behind)");
            }
            EditorMessage::MoveFront => {
                self.end_slider_gesture();
                let n = self.scene.layers().len();
                let mut indices: Vec<usize> = self.selected.iter().copied().collect();
                indices.sort_unstable_by(|a, b| b.cmp(a));
                if indices.is_empty() || indices[0] + 1 >= n {
                    return false;
                }
                self.scene.snapshot_undo();
                let mut new_sel = BTreeSet::new();
                let mut new_primary = self.primary_index;
                for i in indices {
                    if i + 1 >= n {
                        continue;
                    }
                    if let Err(e) = self.scene.move_layer_raw(i, i + 1) {
                        self.fail(&e);
                        return false;
                    }
                    new_sel.insert(i + 1);
                    if self.primary_index == Some(i) {
                        new_primary = Some(i + 1);
                    }
                }
                self.selected = new_sel;
                self.primary_index = new_primary;
                self.after_edit();
                self.ok("moved front (on top)");
            }
            EditorMessage::Undo => {
                if self.scene.undo() {
                    self.after_edit();
                    self.ok("undo");
                }
            }
            EditorMessage::Redo => {
                if self.scene.redo() {
                    self.after_edit();
                    self.ok("redo");
                }
            }
            EditorMessage::Save => match self.scene.save() {
                Ok(()) => {
                    self.ok("saved scene.json · regenerating thumbnail…");
                    if let Some(p) = &self.preview {
                        p.request_reload();
                    }
                    self.schedule_preview_gif();
                    // Kick sooner on explicit save.
                    self.gif_idle_ticks = 20;
                }
                Err(e) => self.fail(&e),
            },
            EditorMessage::RefreshPreview => self.do_reload_preview(),
            EditorMessage::TogglePlay => {
                self.playing = !self.playing;
                if let Some(p) = &self.preview {
                    p.set_playing(self.playing);
                }
                self.ok(if self.playing {
                    "preview playing"
                } else {
                    "preview paused"
                });
            }
            EditorMessage::OpenFolder => {
                let _ = std::process::Command::new("xdg-open")
                    .arg(&self.scene.dir)
                    .spawn();
            }
            EditorMessage::ImportImagePath(s) => self.import_path = s,
            EditorMessage::CommitImport => {
                self.end_slider_gesture();
                let path = self.import_path.trim().to_string();
                if path.is_empty() {
                    self.fail("enter a path to an image");
                } else {
                    match self.scene.import_image(std::path::Path::new(&path)) {
                        Ok(rel) => {
                            self.assets = self.scene.list_assets(400);
                            // Wire to primary image layer when possible.
                            if let Some(i) = self.primary() {
                                let is_image = self
                                    .scene
                                    .layers()
                                    .into_iter()
                                    .find(|l| l.index == i)
                                    .map(|l| l.kind == LayerKind::Image)
                                    .unwrap_or(false);
                                if is_image {
                                    // import_image already push_undo'd via dirty path? it may not —
                                    // set_image_model pushes its own undo. That's fine as two steps.
                                    if let Err(e) = self.scene.set_image_model(i, &rel) {
                                        self.fail(&e);
                                    } else {
                                        self.after_edit();
                                        self.ok(&format!("imported + assigned {rel}"));
                                    }
                                } else {
                                    self.ok(&format!(
                                        "imported {rel} · select an image layer & click asset to assign"
                                    ));
                                }
                            } else {
                                self.ok(&format!("imported {rel} · select layer to assign"));
                            }
                            self.import_path.clear();
                        }
                        Err(e) => self.fail(&e),
                    }
                }
            }
            EditorMessage::RefreshAssets => {
                self.assets = self.scene.list_assets(400);
                self.ok("asset list refreshed");
            }
        }
        false
    }

    fn map_selected_origin(&mut self, axis: usize, value: f32) {
        if self.selected.is_empty() {
            return;
        }
        self.begin_slider_gesture(&format!("origin{axis}"));
        for &i in &self.selected {
            let mut o = self.scene.read_origin(i);
            o[axis] = value;
            let _ = self.scene.set_origin_raw(i, o);
        }
        self.after_edit();
    }

    fn map_selected_scale(&mut self, axis: usize, value: f32) {
        if self.selected.is_empty() {
            return;
        }
        self.begin_slider_gesture(&format!("scale{axis}"));
        for &i in &self.selected {
            let mut o = self.scene.read_scale(i);
            o[axis] = value;
            let _ = self.scene.set_scale_raw(i, o);
        }
        self.after_edit();
    }

    fn map_selected_angle(&mut self, axis: usize, deg: f32) {
        if self.selected.is_empty() {
            return;
        }
        self.begin_slider_gesture(&format!("angle{axis}"));
        for &i in &self.selected {
            let mut a = self.scene.read_angles_degrees(i);
            a[axis] = deg;
            let _ = self.scene.set_angles_degrees_raw(i, a);
        }
        self.after_edit();
    }

    fn map_selected_alpha(&mut self, a: f32) {
        if self.selected.is_empty() {
            return;
        }
        self.begin_slider_gesture("alpha");
        for &i in &self.selected {
            let _ = self.scene.set_alpha_raw(i, a);
        }
        self.after_edit();
    }

    fn map_layer_color(&mut self, axis: usize, v: f32) {
        if axis > 2 || self.selected.is_empty() {
            return;
        }
        self.begin_slider_gesture(&format!("lcol{axis}"));
        for &i in &self.selected {
            let mut c = self.scene.read_color(i).unwrap_or([1.0, 1.0, 1.0]);
            c[axis] = v;
            let _ = self.scene.set_color_raw(i, c);
        }
        self.after_edit();
    }

    fn map_particle(&mut self, key: &str, v: f32) {
        if self.selected.is_empty() {
            return;
        }
        self.begin_slider_gesture(&format!("pov:{key}"));
        for &i in &self.selected {
            let _ = self.scene.set_particle_override_f32_raw(i, key, v);
        }
        self.after_edit();
    }

    fn map_particle_color(&mut self, axis: usize, v: f32) {
        if axis > 2 {
            return;
        }
        if self.selected.is_empty() {
            return;
        }
        self.begin_slider_gesture(&format!("pcol{axis}"));
        for &i in &self.selected {
            let mut c = self.scene.read_particle_override_colorn(i);
            c[axis] = v;
            let _ = self.scene.set_particle_override_color_raw(i, c);
        }
        self.after_edit();
    }

    fn apply_edit<F>(&mut self, f: F)
    where
        F: FnOnce(&mut EditableScene) -> Result<(), String>,
    {
        match f(&mut self.scene) {
            Ok(()) => self.after_edit(),
            Err(e) => self.fail(&e),
        }
    }

    fn after_edit(&mut self) {
        self.status = if self.scene.dirty {
            "modified · preview will refresh…".into()
        } else {
            "ok".into()
        };
        self.status_ok = true;
        self.mark_reload();
        // Keep typed fields in sync after gizmo / multi edits (unless user is mid-type —
        // we only sync when not focusing; simplest: always sync drafts after structural edits).
        self.sync_num_drafts();
        // Debounced auto thumbnail regen for the library grid.
        self.schedule_preview_gif();
    }

    fn ok(&mut self, s: &str) {
        self.status = s.into();
        self.status_ok = true;
    }
    fn fail(&mut self, s: &str) {
        self.status = s.into();
        self.status_ok = false;
    }

    pub fn view(&self) -> Element<'_, EditorMessage> {
        container(
            column![
                toolbar(self),
                hrule(),
                row![
                    container(layer_tree(self))
                        .width(Length::Fixed(pal::LAYERS))
                        .height(Fill)
                        .style(|_| panel(pal::panel())),
                    vrule(),
                    container(center_panel(self))
                        .width(Fill)
                        .height(Fill)
                        .style(|_| panel(pal::bg())),
                    vrule(),
                    container(inspector(self))
                        .width(Length::Fixed(pal::INSPECT))
                        .height(Fill)
                        .style(|_| panel(pal::panel())),
                ]
                .height(Fill),
                hrule(),
                footer(self),
            ]
            .width(Fill)
            .height(Fill),
        )
        .width(Fill)
        .height(Fill)
        .style(|_| panel(pal::bg()))
        .into()
    }
}

// ── UI ──────────────────────────────────────────────────────────────────────

fn toolbar(ed: &EditorSession) -> Element<'_, EditorMessage> {
    let (ow, oh) = ed.scene.ortho();
    let brand = row![
        text("WALL").size(14).color(pal::fg()).font(Font::MONOSPACE),
        text("STUDIO")
            .size(14)
            .color(pal::accent())
            .font(Font::MONOSPACE),
        text("  EDITOR")
            .size(14)
            .color(pal::dim())
            .font(Font::MONOSPACE),
    ]
    .align_y(Alignment::Center);

    let play_label = if ed.playing { "Pause" } else { "Play" };

    container(
        row![
            brand,
            Space::new().width(12),
            text(format!("{}×{}", ow as i32, oh as i32))
                .size(11)
                .color(pal::mute())
                .font(Font::MONOSPACE),
            Space::new().width(8),
            text(truncate(&ed.scene.title, 28))
                .size(12)
                .color(pal::fg()),
            Space::new().width(16),
            tool_chip("V", Tool::Select, ed.tool),
            tool_chip("W", Tool::Move, ed.tool),
            tool_chip("E", Tool::Scale, ed.tool),
            tool_chip("R", Tool::Rotate, ed.tool),
            Space::new().width(Fill),
            tool_btn("Undo", EditorMessage::Undo, ed.scene.can_undo()),
            tool_btn("Redo", EditorMessage::Redo, ed.scene.can_redo()),
            Space::new().width(8),
            tool_btn("Save", EditorMessage::Save, true),
            tool_btn("Refresh", EditorMessage::RefreshPreview, true),
            tool_btn(play_label, EditorMessage::TogglePlay, true),
            Space::new().width(8),
            tool_btn("Folder", EditorMessage::OpenFolder, true),
            tool_btn("Close", EditorMessage::Close, true),
        ]
        .align_y(Alignment::Center)
        .spacing(4)
        .padding(Padding::from([8, 12])),
    )
    .width(Fill)
    .style(|_| panel(pal::panel()))
    .into()
}

fn tool_chip(key: &str, tool: Tool, active: Tool) -> Element<'_, EditorMessage> {
    let on = active == tool;
    button(
        text(format!(
            "{key} {}",
            tool.label().split(' ').next().unwrap_or("")
        ))
        .size(11)
        .font(Font::MONOSPACE),
    )
    .padding([6, 10])
    .on_press(EditorMessage::SetTool(tool))
    .style(move |_, status| {
        let bg = if on {
            Color::from_rgb(0.22, 0.20, 0.14)
        } else if matches!(status, iced::widget::button::Status::Hovered) {
            pal::panel2()
        } else {
            Color::TRANSPARENT
        };
        iced::widget::button::Style {
            background: Some(Background::Color(bg)),
            text_color: if on { pal::accent() } else { pal::dim() },
            border: Border {
                color: if on { pal::accent() } else { pal::line() },
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn layer_tree(ed: &EditorSession) -> Element<'_, EditorMessage> {
    let layers = ed.scene.layers();
    let n = layers.len();
    let nsel = ed.selected.len();
    let mut col = column![
        text("EXPLORER")
            .size(11)
            .color(pal::accent())
            .font(Font::MONOSPACE),
        text(format!("{n} objects · {nsel} selected"))
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        text("LMB select · RMB multi · Ctrl/Shift · eye = visible")
            .size(9)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        text("Z-order: Back/Front · later in list draws on top")
            .size(9)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        Space::new().height(6),
        row![
            small_btn("Back", EditorMessage::MoveBack),
            small_btn("Front", EditorMessage::MoveFront),
            small_btn("Dup", EditorMessage::Duplicate),
            small_btn("Del", EditorMessage::Delete),
            small_btn("All", EditorMessage::SelectAll),
            small_btn("∅", EditorMessage::ClearSelection),
        ]
        .spacing(4),
        Space::new().height(8),
    ]
    .spacing(2)
    .padding(10)
    .width(Fill);

    let mut list = column![].spacing(1).width(Fill);
    for l in layers {
        let sel = ed.selected.contains(&l.index);
        list = list.push(layer_row(sel, l));
    }
    col = col.push(scrollable(list).height(Fill));
    col.into()
}

fn layer_row(selected: bool, l: LayerSummary) -> Element<'static, EditorMessage> {
    let indent = 4.0 + l.depth as f32 * 14.0;
    let kind = match l.kind {
        LayerKind::Image => "IMG",
        LayerKind::Particle => "FX",
        LayerKind::Text => "TXT",
        LayerKind::Sound => "SND",
        LayerKind::Group => "GRP",
        LayerKind::Other => "···",
    };
    let mut badges = String::new();
    if l.scripted {
        badges.push_str("S");
    }
    if l.animated {
        badges.push_str("A");
    }
    if l.has_effects {
        badges.push_str("E");
    }
    let name = truncate(&l.name, 26);
    let idx = l.index;
    let visible = l.visible;
    let id_label = format!("#{}", l.id);

    // Checkbox MUST sit outside the select hit-target — nested inside a button
    // steals clicks and only the first row often works.
    let eye = checkbox(visible)
        .on_toggle(move |v| EditorMessage::ToggleVisible(idx, v))
        .size(16);

    let label = row![
        text(kind)
            .size(10)
            .color(pal::accent())
            .font(Font::MONOSPACE)
            .width(Length::Fixed(28.0)),
        column![
            text(name)
                .size(12)
                .color(if selected { pal::fg() } else { pal::dim() }),
            text(id_label)
                .size(9)
                .color(pal::mute())
                .font(Font::MONOSPACE),
        ]
        .spacing(0),
        Space::new().width(Fill),
        text(badges)
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .width(Fill)
    .padding(Padding {
        top: 4.0,
        right: 6.0,
        bottom: 4.0,
        left: 4.0,
    });

    // Left = select (Ctrl/Shift via host mods). Right = always multi-toggle.
    let hit = mouse_area(
        container(label)
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(if selected {
                    pal::sel()
                } else {
                    Color::TRANSPARENT
                })),
                border: Border {
                    color: if selected {
                        pal::sel_border()
                    } else {
                        Color::TRANSPARENT
                    },
                    width: if selected { 1.0 } else { 0.0 },
                    radius: 0.0.into(),
                },
                ..Default::default()
            }),
    )
    .interaction(mouse::Interaction::Pointer)
    .on_press(EditorMessage::SelectLayer(idx))
    .on_right_press(EditorMessage::ToggleLayer(idx));

    container(
        row![eye, hit]
            .spacing(6)
            .align_y(Alignment::Center)
            .width(Fill)
            .padding(Padding {
                top: 2.0,
                right: 4.0,
                bottom: 2.0,
                left: indent,
            }),
    )
    .width(Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(if selected {
            Color::TRANSPARENT // already painted on hit
        } else {
            Color::TRANSPARENT
        })),
        ..Default::default()
    })
    .into()
}

fn center_panel(ed: &EditorSession) -> Element<'_, EditorMessage> {
    let (ow, oh) = ed.scene.ortho();
    let meta = if let Some(p) = &ed.preview {
        let (pw, ph) = p.width_height();
        let cur = ed
            .cursor_ortho
            .map(|c| format!(" · cursor {:.0},{:.0}", c[0], c[1]))
            .unwrap_or_default();
        format!(
            "{}×{} · scene {}×{} · {} · {} frames · GPU · {}{}",
            pw,
            ph,
            ow as i32,
            oh as i32,
            if ed.playing { "LIVE" } else { "paused" },
            p.frame_count(),
            ed.tool.label(),
            cur,
        )
    } else {
        "preview unavailable — is walld running?".into()
    };

    let pivot = ed.selection_pivot();
    let (ow, oh) = ed.scene.ortho();
    let has_sel = !ed.selected.is_empty();
    let tool = ed.tool;
    let dragging = ed.drag.is_some();

    let canvas: Element<'_, EditorMessage> = if let Some(ref h) = ed.preview_handle {
        let handle = h.clone();
        responsive(move |size| {
            let w = size.width.max(1.0);
            let h = size.height.max(1.0);
            let img = container(
                image(handle.clone())
                    .width(Fill)
                    .height(Fill)
                    .content_fit(ContentFit::Contain),
            )
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.04, 0.04, 0.05))),
                border: Border {
                    color: if dragging { pal::accent() } else { pal::line() },
                    width: if dragging { 2.0 } else { 1.0 },
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

            // Visual gizmo on top of the live preview (Roblox-style axes).
            let giz = gizmo::gizmo_element(tool, has_sel, pivot, [ow, oh], w, h);

            stack![img, giz].width(Fill).height(Fill).into()
        })
        .into()
    } else {
        container(
            column![
                text("Loading GPU preview…").size(16).color(pal::dim()),
                text(&ed.status).size(12).color(pal::mute()),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .style(|_| panel(pal::panel2()))
        .into()
    };

    column![
        row![
            text("PREVIEW")
                .size(11)
                .color(pal::accent())
                .font(Font::MONOSPACE),
            Space::new().width(Fill),
            text(meta).size(10).color(pal::mute()).font(Font::MONOSPACE),
        ]
        .padding(Padding {
            top: 8.0,
            right: 12.0,
            bottom: 4.0,
            left: 12.0,
        }),
        container(
            text(match ed.tool {
                Tool::Select => "Select: click scene to pick · Ctrl multi in Explorer",
                Tool::Move => "Move gizmo: drag center (free) · red X · green Y",
                Tool::Scale => "Scale gizmo: blue corner = uniform · red/green = axis",
                Tool::Rotate => "Rotate gizmo: drag the ring / yellow knob",
            })
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        )
        .padding(Padding {
            top: 0.0,
            right: 12.0,
            bottom: 4.0,
            left: 12.0,
        }),
        container(canvas).width(Fill).height(Fill).padding(Padding {
            top: 4.0,
            right: 12.0,
            bottom: 8.0,
            left: 12.0,
        }),
        assets_strip(ed),
    ]
    .width(Fill)
    .height(Fill)
    .into()
}

fn assets_strip(ed: &EditorSession) -> Element<'_, EditorMessage> {
    let n = ed.assets.len();
    // Show a few assignable image-like assets as clickable chips.
    let mut chips = row![].spacing(4);
    let mut shown = 0usize;
    for a in &ed.assets {
        let lower = a.to_ascii_lowercase();
        let is_img = lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".webp")
            || lower.ends_with(".tga")
            || lower.contains("materials/")
            || lower.contains("models/");
        if !is_img {
            continue;
        }
        let path = a.clone();
        let label = truncate(a, 28);
        chips = chips.push(
            button(text(label).size(10).font(Font::MONOSPACE))
                .padding([3, 6])
                .on_press(EditorMessage::AssignImage(path))
                .style(|_, status| flat_btn_style(status)),
        );
        shown += 1;
        if shown >= 8 {
            break;
        }
    }

    column![
        row![
            text(format!("ASSETS · {n} · click to assign to image layer"))
                .size(10)
                .color(pal::mute())
                .font(Font::MONOSPACE),
            Space::new().width(Fill),
            small_btn("Refresh", EditorMessage::RefreshAssets),
        ]
        .padding(Padding {
            top: 0.0,
            right: 12.0,
            bottom: 0.0,
            left: 12.0,
        }),
        container(chips.wrap())
            .padding(Padding {
                top: 2.0,
                right: 12.0,
                bottom: 2.0,
                left: 12.0,
            })
            .width(Fill),
        row![
            text_input("/path/to/image.png", &ed.import_path)
                .on_input(EditorMessage::ImportImagePath)
                .on_submit(EditorMessage::CommitImport)
                .padding(6)
                .size(11)
                .width(Fill),
            Space::new().width(6),
            small_btn("Import→layer", EditorMessage::CommitImport),
        ]
        .padding(Padding {
            top: 4.0,
            right: 12.0,
            bottom: 8.0,
            left: 12.0,
        })
        .align_y(Alignment::Center),
    ]
    .width(Fill)
    .into()
}

fn inspector(ed: &EditorSession) -> Element<'_, EditorMessage> {
    if ed.selected.is_empty() {
        return container(
            column![
                text("Nothing selected").size(13).color(pal::dim()),
                text("Click a layer in Explorer, or click the preview with Select tool.")
                    .size(11)
                    .color(pal::mute()),
            ]
            .spacing(6),
        )
        .padding(16)
        .into();
    }

    let multi = ed.selected.len() > 1;
    let i = ed.primary().unwrap();
    let layers = ed.scene.layers();
    let layer = layers.iter().find(|l| l.index == i);

    let origin = ed.scene.read_origin(i);
    let scale = ed.scene.read_scale(i);
    let angles = ed.scene.read_angles_degrees(i);
    let alpha = ed.scene.read_alpha(i);
    let brightness = ed.scene.read_brightness(i);
    let (ow, oh) = ed.scene.ortho();

    let header = if multi {
        format!("{} layers selected · editing all", ed.selected.len())
    } else {
        layer
            .map(|l| format!("#{} · {}", l.id, l.kind.as_label()))
            .unwrap_or_else(|| format!("#{i}"))
    };

    let mut col = column![
        text("PROPERTIES")
            .size(11)
            .color(pal::accent())
            .font(Font::MONOSPACE),
        text(header)
            .size(11)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        Space::new().height(8),
    ]
    .spacing(6)
    .padding(12)
    .width(Fill);

    if !multi {
        col = col.push(text("Name").size(11).color(pal::dim()));
        col = col.push(
            text_input("layer name", &ed.rename_draft)
                .on_input(EditorMessage::Rename)
                .on_submit(EditorMessage::CommitRename)
                .padding(6)
                .size(13),
        );
        col = col.push(Space::new().height(8));
    } else {
        col = col.push(
            text("Transforms apply to every selected layer.")
                .size(11)
                .color(pal::accent()),
        );
        col = col.push(Space::new().height(6));
    }

    // Fixed slider tracks (do NOT expand with current value — that recenters the
    // thumb and lets values run away). Type into the box to go past the track.
    let ox_max = ow.max(1.0);
    let oy_max = oh.max(1.0);
    col = col.push(section("DRAW ORDER / Z"));
    col = col.push(
        text(format!("Index {i} · Back = behind · Front = on top"))
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
    );
    col = col.push(
        row![
            small_btn("Back", EditorMessage::MoveBack),
            small_btn("Front", EditorMessage::MoveFront),
        ]
        .spacing(6),
    );

    col = col.push(Space::new().height(6));
    col = col.push(section("TRANSFORM"));
    col = col.push(num_prop(
        ed,
        "Origin X",
        "ox",
        origin[0],
        -ox_max * 0.25,
        ox_max * 1.25,
        EditorMessage::SetOriginX,
    ));
    col = col.push(num_prop(
        ed,
        "Origin Y",
        "oy",
        origin[1],
        -oy_max * 0.25,
        oy_max * 1.25,
        EditorMessage::SetOriginY,
    ));
    col = col.push(num_prop(
        ed,
        "Origin Z",
        "oz",
        origin[2],
        -1000.0,
        1000.0,
        EditorMessage::SetOriginZ,
    ));
    col = col.push(num_prop(
        ed,
        "Scale X",
        "sx",
        scale[0],
        0.01,
        4.0,
        EditorMessage::SetScaleX,
    ));
    col = col.push(num_prop(
        ed,
        "Scale Y",
        "sy",
        scale[1],
        0.01,
        4.0,
        EditorMessage::SetScaleY,
    ));
    col = col.push(num_prop(
        ed,
        "Scale Z",
        "sz",
        scale[2],
        0.01,
        4.0,
        EditorMessage::SetScaleZ,
    ));
    col = col.push(num_prop(
        ed,
        "Angle X°",
        "ax",
        angles[0],
        -180.0,
        180.0,
        EditorMessage::SetAngleX,
    ));
    col = col.push(num_prop(
        ed,
        "Angle Y°",
        "ay",
        angles[1],
        -180.0,
        180.0,
        EditorMessage::SetAngleY,
    ));
    col = col.push(num_prop(
        ed,
        "Angle Z°",
        "az",
        angles[2],
        -180.0,
        180.0,
        EditorMessage::SetAngleZ,
    ));
    col = col.push(num_prop(
        ed,
        "Alpha",
        "alpha",
        alpha,
        0.0,
        1.0,
        EditorMessage::SetAlpha,
    ));
    if !multi {
        col = col.push(num_prop(
            ed,
            "Brightness",
            "bright",
            brightness,
            0.0,
            2.0,
            EditorMessage::SetBrightness,
        ));
        let lcol = ed.scene.read_color(i).unwrap_or([1.0, 1.0, 1.0]);
        col = col.push(num_prop_owned(
            ed,
            "Color R".into(),
            "lcolr".into(),
            lcol[0],
            0.0,
            2.0,
            |v| EditorMessage::SetLayerColor(0, v),
        ));
        col = col.push(num_prop_owned(
            ed,
            "Color G".into(),
            "lcolg".into(),
            lcol[1],
            0.0,
            2.0,
            |v| EditorMessage::SetLayerColor(1, v),
        ));
        col = col.push(num_prop_owned(
            ed,
            "Color B".into(),
            "lcolb".into(),
            lcol[2],
            0.0,
            2.0,
            |v| EditorMessage::SetLayerColor(2, v),
        ));
    }

    if !multi {
        if let Some(layer) = layer {
            if layer.kind == LayerKind::Particle || layer.has_particle_override {
                col = col.push(Space::new().height(8));
                col = col.push(section("INSTANCE OVERRIDE"));
                if let Some(p) = ed.scene.read_particle_path(i) {
                    col = col.push(text(p).size(10).color(pal::mute()).font(Font::MONOSPACE));
                }
                col = col.push(num_prop(
                    ed,
                    "Rate",
                    "rate",
                    ed.scene.read_particle_override_f32(i, "rate"),
                    0.0,
                    3.0,
                    EditorMessage::SetParticleRate,
                ));
                col = col.push(num_prop(
                    ed,
                    "Speed",
                    "speed",
                    ed.scene.read_particle_override_f32(i, "speed"),
                    0.0,
                    3.0,
                    EditorMessage::SetParticleSpeed,
                ));
                col = col.push(num_prop(
                    ed,
                    "Size",
                    "size",
                    ed.scene.read_particle_override_f32(i, "size"),
                    0.0,
                    3.0,
                    EditorMessage::SetParticleSize,
                ));
                col = col.push(num_prop(
                    ed,
                    "Alpha",
                    "palpha",
                    ed.scene.read_particle_override_f32(i, "alpha"),
                    0.0,
                    2.0,
                    EditorMessage::SetParticleAlpha,
                ));
                col = col.push(num_prop(
                    ed,
                    "Count",
                    "count",
                    ed.scene.read_particle_override_f32(i, "count"),
                    0.0,
                    3.0,
                    EditorMessage::SetParticleCount,
                ));
                col = col.push(num_prop(
                    ed,
                    "Lifetime",
                    "lifetime",
                    ed.scene.read_particle_override_f32(i, "lifetime"),
                    0.0,
                    3.0,
                    EditorMessage::SetParticleLifetime,
                ));
                let coln = ed.scene.read_particle_override_colorn(i);
                col = col.push(num_prop_owned(
                    ed,
                    "Color R".into(),
                    "pcolr".into(),
                    coln[0],
                    0.0,
                    2.0,
                    |v| EditorMessage::SetParticleColorN(0, v),
                ));
                col = col.push(num_prop_owned(
                    ed,
                    "Color G".into(),
                    "pcolg".into(),
                    coln[1],
                    0.0,
                    2.0,
                    |v| EditorMessage::SetParticleColorN(1, v),
                ));
                col = col.push(num_prop_owned(
                    ed,
                    "Color B".into(),
                    "pcolb".into(),
                    coln[2],
                    0.0,
                    2.0,
                    |v| EditorMessage::SetParticleColorN(2, v),
                ));

                // Full particle system document — every emitter / initializer /
                // operator / renderer / control-point field.
                let fields = ed.scene.particle_fields(i);
                let addable = ed.scene.particle_addable_keys(i);
                let slots = ed.scene.particle_slots(i);
                if fields.is_empty() && addable.is_empty() {
                    col = col.push(Space::new().height(6));
                    col = col.push(
                        text("Particle JSON not loaded — reselect the layer.")
                            .size(10)
                            .color(pal::mute()),
                    );
                } else {
                    // Group fields + addable keys by section.
                    let mut sections: Vec<String> = Vec::new();
                    for f in &fields {
                        if !sections.contains(&f.section) {
                            sections.push(f.section.clone());
                        }
                    }
                    for a in &addable {
                        if !sections.contains(&a.section) {
                            sections.push(a.section.clone());
                        }
                    }
                    // Ensure slots without fields still appear (empty block).
                    for (_, sec) in &slots {
                        if !sections.contains(sec) {
                            sections.push(sec.clone());
                        }
                    }

                    for sec in sections {
                        col = col.push(Space::new().height(8));
                        col = col.push(section(sec.clone()));

                        for field in fields.iter().filter(|f| f.section == sec) {
                            match &field.kind {
                                ParticleFieldKind::Float { value, lo, hi } => {
                                    let key = field.key.clone();
                                    let key2 = field.key.clone();
                                    col = col.push(num_prop_owned(
                                        ed,
                                        field.label.clone(),
                                        key,
                                        *value,
                                        *lo,
                                        *hi,
                                        move |v| EditorMessage::ParticleDocFloat(key2.clone(), v),
                                    ));
                                }
                                ParticleFieldKind::Text { value: _ } => {
                                    let key = field.key.clone();
                                    let key_in = field.key.clone();
                                    let key_go = field.key.clone();
                                    let draft = ed.draft(&key);
                                    col = col.push(
                                        column![
                                            text(field.label.clone())
                                                .size(11)
                                                .color(pal::dim())
                                                .width(Length::Fixed(88.0)),
                                            text_input("", &draft)
                                                .on_input(move |s| {
                                                    EditorMessage::NumDraft(key_in.clone(), s)
                                                })
                                                .on_submit(EditorMessage::ParticleDocTextCommit(
                                                    key_go
                                                ),)
                                                .padding(4)
                                                .size(11)
                                                .width(Fill)
                                                .font(Font::MONOSPACE),
                                        ]
                                        .spacing(2),
                                    );
                                }
                            }
                        }

                        // Known missing keys as compact "+ label" buttons.
                        let missing: Vec<&ParticleAddableKey> =
                            addable.iter().filter(|a| a.section == sec).collect();
                        if !missing.is_empty() {
                            col = col.push(
                                text("Add key")
                                    .size(10)
                                    .color(pal::mute())
                                    .font(Font::MONOSPACE),
                            );
                            let mut row_btns = row![].spacing(4);
                            let mut count_in_row = 0;
                            for a in missing {
                                let label = format!("+ {}", a.label);
                                let key = a.key.clone();
                                let def = a.default.to_string();
                                let btn = button(text(label).size(10).font(Font::MONOSPACE))
                                    .padding([3, 6])
                                    .on_press(EditorMessage::ParticleAddKey(key, def))
                                    .style(|_, status| flat_btn_style(status));
                                row_btns = row_btns.push(btn);
                                count_in_row += 1;
                                if count_in_row >= 3 {
                                    col = col.push(row_btns.wrap());
                                    row_btns = row![].spacing(4);
                                    count_in_row = 0;
                                }
                            }
                            if count_in_row > 0 {
                                col = col.push(row_btns.wrap());
                            }
                        }

                        // Freeform key entry for this section's slot.
                        if let Some((slot, _)) = slots.iter().find(|(_, s)| *s == sec) {
                            let draft_key = format!("addkey:{slot}");
                            let draft = ed.draft(&draft_key);
                            let slot_owned = slot.clone();
                            let slot_go = slot.clone();
                            let key_in = draft_key.clone();
                            col = col.push(
                                row![
                                    text_input("custom key…", &draft)
                                        .on_input(move |s| {
                                            EditorMessage::NumDraft(key_in.clone(), s)
                                        })
                                        .on_submit(EditorMessage::ParticleAddCustom(
                                            slot_owned.clone(),
                                        ))
                                        .padding(4)
                                        .size(11)
                                        .width(Fill)
                                        .font(Font::MONOSPACE),
                                    Space::new().width(4),
                                    button(text("+").size(12).font(Font::MONOSPACE))
                                        .padding([4, 10])
                                        .on_press(EditorMessage::ParticleAddCustom(slot_go))
                                        .style(|_, status| flat_btn_style(status)),
                                ]
                                .align_y(Alignment::Center),
                            );
                        }
                    }
                }
            }

            if layer.kind == LayerKind::Image {
                col = col.push(Space::new().height(8));
                col = col.push(section("IMAGE"));
                if let Some(m) = ed.scene.read_image_model(i) {
                    col = col.push(
                        text(m.clone())
                            .size(10)
                            .color(pal::mute())
                            .font(Font::MONOSPACE),
                    );
                } else {
                    col = col.push(text("(no image path)").size(10).color(pal::mute()));
                }
                col = col.push(
                    text("Import below or AssignImage from assets path.")
                        .size(10)
                        .color(pal::mute()),
                );
                // Quick path field reuse: type relative path + Enter via num draft
                let draft = ed.draft("imgpath");
                col = col.push(
                    row![
                        text_input("materials/… or models/…", &draft)
                            .on_input(|s| EditorMessage::NumDraft("imgpath".into(), s))
                            .on_submit(EditorMessage::NumCommit("imgpath".into()))
                            .padding(4)
                            .size(11)
                            .width(Fill)
                            .font(Font::MONOSPACE),
                        Space::new().width(4),
                        button(text("Set").size(10).font(Font::MONOSPACE))
                            .padding([4, 8])
                            .on_press({
                                let p = ed.draft("imgpath");
                                EditorMessage::AssignImage(p)
                            })
                            .style(|_, status| flat_btn_style(status)),
                    ]
                    .align_y(Alignment::Center),
                );
            }

            if layer.kind == LayerKind::Text {
                col = col.push(Space::new().height(8));
                col = col.push(section("TEXT"));
                col = col.push(
                    text_input("literal text…", &ed.text_draft)
                        .on_input(EditorMessage::TextDraft)
                        .on_submit(EditorMessage::CommitText)
                        .padding(6)
                        .size(13),
                );
                col = col.push(small_btn("Apply text", EditorMessage::CommitText));
            }

            let effects = ed.scene.effects_on(i);
            if !effects.is_empty() {
                col = col.push(Space::new().height(8));
                col = col.push(section("EFFECTS"));
                for ef in effects {
                    let label = if ef.name.is_empty() {
                        ef.file.clone()
                    } else {
                        format!("{} · {}", ef.name, ef.file)
                    };
                    col = col.push(
                        checkbox(ef.visible)
                            .label(truncate(&label, 36))
                            .on_toggle(move |v| EditorMessage::EffectVisible(ef.index, v))
                            .size(14)
                            .text_size(11),
                    );
                    for c in ef.constants {
                        let ei = ef.index;
                        let pass = c.pass;
                        match c.value {
                            EffectConstValue::Float(f) => {
                                let key = c.key.clone();
                                let label = c.key.clone();
                                let draft_key = format!("fx:{ei}:{pass}:{key}");
                                let (lo, hi) = fixed_effect_range(f);
                                let key2 = key;
                                col = col.push(num_prop_owned(
                                    ed,
                                    label,
                                    draft_key,
                                    f,
                                    lo,
                                    hi,
                                    move |v| EditorMessage::EffectConst(ei, pass, key2.clone(), v),
                                ));
                            }
                            EffectConstValue::Vec2(v) => {
                                for (axis, val) in v.iter().enumerate() {
                                    let key = c.key.clone();
                                    let label = format!("{}[{axis}]", c.key);
                                    let draft_key = format!("fx:{ei}:{pass}:{}:{axis}", c.key);
                                    let (lo, hi) = fixed_effect_range(*val);
                                    let key2 = key;
                                    col = col.push(num_prop_owned(
                                        ed,
                                        label,
                                        draft_key,
                                        *val,
                                        lo,
                                        hi,
                                        move |v| {
                                            EditorMessage::EffectConstComp(
                                                ei,
                                                pass,
                                                key2.clone(),
                                                axis,
                                                v,
                                            )
                                        },
                                    ));
                                }
                            }
                            EffectConstValue::Vec3(v) => {
                                let axes = ["x", "y", "z"];
                                for (axis, val) in v.iter().enumerate() {
                                    let key = c.key.clone();
                                    let label = format!("{} {}", c.key, axes[axis]);
                                    let draft_key = format!("fx:{ei}:{pass}:{}:{axis}", c.key);
                                    let (lo, hi) = fixed_effect_range(*val);
                                    let key2 = key;
                                    col = col.push(num_prop_owned(
                                        ed,
                                        label,
                                        draft_key,
                                        *val,
                                        lo,
                                        hi,
                                        move |v| {
                                            EditorMessage::EffectConstComp(
                                                ei,
                                                pass,
                                                key2.clone(),
                                                axis,
                                                v,
                                            )
                                        },
                                    ));
                                }
                            }
                            EffectConstValue::Vec4(v) => {
                                let axes = ["x", "y", "z", "w"];
                                for (axis, val) in v.iter().enumerate() {
                                    let key = c.key.clone();
                                    let label = format!("{} {}", c.key, axes[axis]);
                                    let draft_key = format!("fx:{ei}:{pass}:{}:{axis}", c.key);
                                    let (lo, hi) = fixed_effect_range(*val);
                                    let key2 = key;
                                    col = col.push(num_prop_owned(
                                        ed,
                                        label,
                                        draft_key,
                                        *val,
                                        lo,
                                        hi,
                                        move |v| {
                                            EditorMessage::EffectConstComp(
                                                ei,
                                                pass,
                                                key2.clone(),
                                                axis,
                                                v,
                                            )
                                        },
                                    ));
                                }
                            }
                            EffectConstValue::Text(s) => {
                                let key = c.key.clone();
                                let draft_key = format!("fx:{ei}:{pass}:{key}");
                                let draft = ed.draft(&draft_key);
                                let _ = s;
                                let key_in = draft_key.clone();
                                let key_go = draft_key.clone();
                                col = col.push(
                                    column![
                                        text(c.key.clone())
                                            .size(11)
                                            .color(pal::dim())
                                            .width(Length::Fixed(88.0)),
                                        text_input("", &draft)
                                            .on_input(move |t| {
                                                EditorMessage::NumDraft(key_in.clone(), t)
                                            })
                                            .on_submit(EditorMessage::NumCommit(key_go))
                                            .padding(4)
                                            .size(11)
                                            .width(Fill)
                                            .font(Font::MONOSPACE),
                                    ]
                                    .spacing(2),
                                );
                            }
                            EffectConstValue::Other => {
                                col = col.push(
                                    text(format!("{} (unsupported)", c.key))
                                        .size(10)
                                        .color(pal::mute()),
                                );
                            }
                        }
                    }
                }
            }

            if layer.scripted {
                col = col.push(Space::new().height(10));
                col = col.push(
                    text("Scripted props may override static edits each frame.")
                        .size(11)
                        .color(pal::accent()),
                );
            }
        }
    }

    scrollable(col).height(Fill).into()
}

fn footer(ed: &EditorSession) -> Element<'_, EditorMessage> {
    let color = if ed.status_ok { pal::dim() } else { pal::err() };
    container(
        row![
            text(&ed.status).size(11).color(color).font(Font::MONOSPACE),
            Space::new().width(Fill),
            text(format!("{} sel", ed.selected.len()))
                .size(11)
                .color(pal::mute())
                .font(Font::MONOSPACE),
            Space::new().width(12),
            text(if ed.scene.dirty { "dirty" } else { "saved" })
                .size(11)
                .color(if ed.scene.dirty {
                    pal::accent()
                } else {
                    pal::ok()
                })
                .font(Font::MONOSPACE),
        ]
        .padding(Padding::from([6, 12])),
    )
    .width(Fill)
    .style(|_| panel(pal::panel()))
    .into()
}

fn section(label: impl Into<String>) -> Element<'static, EditorMessage> {
    text(label.into())
        .size(10)
        .color(pal::accent())
        .font(Font::MONOSPACE)
        .into()
}

/// Fixed effect slider band from a *snapshot* of the value — not live-expanding.
fn fixed_effect_range(sample: f32) -> (f32, f32) {
    let a = sample.abs();
    if a <= 1.0 {
        (-1.0, 1.0)
    } else if a <= 5.0 {
        (-5.0, 5.0)
    } else if a <= 20.0 {
        (-20.0, 20.0)
    } else {
        (-100.0, 100.0)
    }
}

/// Label + typeable box + fixed-range slider.
/// Slider is clamped to the track; the text box can hold any finite value.
fn num_prop<'a>(
    ed: &'a EditorSession,
    label: &'a str,
    key: &'a str,
    value: f32,
    min: f32,
    max: f32,
    on_slider: fn(f32) -> EditorMessage,
) -> Element<'a, EditorMessage> {
    let hi = if max <= min { min + 1.0 } else { max };
    let lo = min.min(hi - 0.001);
    let draft = ed.draft(key);
    let over = value < lo || value > hi;
    let key_owned = key.to_string();
    let key_commit = key.to_string();
    let step = ((hi - lo) / 400.0).max(0.0001);

    column![
        row![
            text(label)
                .size(11)
                .color(pal::dim())
                .width(Length::Fixed(88.0)),
            text_input("0", &draft)
                .on_input(move |s| EditorMessage::NumDraft(key_owned.clone(), s))
                .on_submit(EditorMessage::NumCommit(key_commit))
                .padding(4)
                .size(12)
                .width(Length::Fixed(88.0))
                .font(Font::MONOSPACE),
            Space::new().width(6),
            text(if over { "typed" } else { "" })
                .size(9)
                .color(pal::accent())
                .font(Font::MONOSPACE),
        ]
        .align_y(Alignment::Center),
        slider(lo..=hi, value.clamp(lo, hi), on_slider).step(step),
    ]
    .spacing(3)
    .into()
}

fn num_prop_owned<'a>(
    ed: &'a EditorSession,
    label: String,
    draft_key: String,
    value: f32,
    min: f32,
    max: f32,
    on_slider: impl Fn(f32) -> EditorMessage + 'a,
) -> Element<'a, EditorMessage> {
    let hi = if max <= min { min + 1.0 } else { max };
    let lo = min.min(hi - 0.001);
    let draft = ed.draft(&draft_key);
    let over = value < lo || value > hi;
    let key_in = draft_key.clone();
    let key_go = draft_key;
    let step = ((hi - lo) / 400.0).max(0.0001);

    column![
        row![
            text(label)
                .size(11)
                .color(pal::dim())
                .width(Length::Fixed(88.0)),
            text_input("0", &draft)
                .on_input(move |s| EditorMessage::NumDraft(key_in.clone(), s))
                .on_submit(EditorMessage::NumCommit(key_go))
                .padding(4)
                .size(12)
                .width(Length::Fixed(88.0))
                .font(Font::MONOSPACE),
            Space::new().width(6),
            text(if over { "typed" } else { "" })
                .size(9)
                .color(pal::accent())
                .font(Font::MONOSPACE),
        ]
        .align_y(Alignment::Center),
        slider(lo..=hi, value.clamp(lo, hi), on_slider).step(step),
    ]
    .spacing(3)
    .into()
}

fn tool_btn(label: &str, msg: EditorMessage, enabled: bool) -> Element<'_, EditorMessage> {
    let mut b = button(text(label).size(11).font(Font::MONOSPACE)).padding([6, 10]);
    if enabled {
        b = b.on_press(msg);
    }
    b.style(|_, status| flat_btn_style(status)).into()
}

fn small_btn(label: &str, msg: EditorMessage) -> Element<'_, EditorMessage> {
    button(text(label).size(10).font(Font::MONOSPACE))
        .padding([4, 8])
        .on_press(msg)
        .style(|_, status| flat_btn_style(status))
        .into()
}

fn flat_btn_style(status: iced::widget::button::Status) -> iced::widget::button::Style {
    let bg = match status {
        iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed => {
            pal::panel2()
        }
        _ => Color::TRANSPARENT,
    };
    iced::widget::button::Style {
        background: Some(Background::Color(bg)),
        text_color: pal::fg(),
        border: Border {
            color: pal::line(),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn panel(c: Color) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(c)),
        ..Default::default()
    }
}

fn hrule() -> Element<'static, EditorMessage> {
    rule::horizontal(1)
        .style(|_| rule::Style {
            color: pal::line(),
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })
        .into()
}

fn vrule() -> Element<'static, EditorMessage> {
    rule::vertical(1)
        .style(|_| rule::Style {
            color: pal::line(),
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })
        .into()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

#[allow(dead_code)]
fn _theme() -> Theme {
    Theme::Dark
}
