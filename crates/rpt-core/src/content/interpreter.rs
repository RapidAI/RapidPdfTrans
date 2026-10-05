//! Graphics-state content-stream interpreter.
//!
//! A bad operator records a diagnostic and the rest of the page continues.
//! Text is recorded glyph by glyph, including invisible (`Tr` 3), clipped,
//! and unmapped glyphs. Form XObjects, annotation appearances, and Type3
//! char procs are visited with their own resources and matrices.

use std::collections::{HashMap, HashSet};

use lopdf::{Document, Object, ObjectId};

use crate::color::Color;
use crate::geom::{Matrix, Rect};
use crate::glyph::{Diagnostic, Disposition, Glyph, GlyphSource, PageInfo, SourceKind};
use crate::pdfutil::{array_of, as_f32, as_name, deref, dict_of, object_id_string, stream_bytes};
use crate::resources::Resources;
use crate::{font::load_font, font::DecodedGlyph, font::Font};

use super::lexer::{lex, ContentOp, Operand};

const DEFAULT_STREAM_LIMIT: usize = 32 * 1024 * 1024;
const DIAGNOSTIC_CAP: usize = 200;

#[derive(Clone, Debug)]
pub struct InterpretOptions {
    pub max_depth: u32,
    pub max_stream_bytes: usize,
    /// Stop after this many pages. `None` reads the whole document.
    pub max_pages: Option<u32>,
}

impl Default for InterpretOptions {
    fn default() -> Self {
        Self {
            max_depth: 32,
            max_stream_bytes: DEFAULT_STREAM_LIMIT,
            max_pages: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Interpreted {
    pub pages: Vec<PageInfo>,
    pub glyphs: Vec<Glyph>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn interpret_document(doc: &Document, opts: &InterpretOptions) -> Interpreted {
    let mut pages = Vec::new();
    let mut glyphs = Vec::new();
    let mut diagnostics = Vec::new();
    let page_map = doc.get_pages();
    if page_map.is_empty() {
        diagnostics.push(Diagnostic {
            page_index: None,
            message: "document has no pages".into(),
        });
    }
    for (number, page_id) in page_map {
        let index = number.saturating_sub(1);
        if opts.max_pages.is_some_and(|limit| index >= limit) {
            break;
        }
        let media_box =
            inherited_rect(doc, page_id, b"MediaBox").unwrap_or(Rect::new(0.0, 0.0, 612.0, 792.0));
        let rotate = inherited_int(doc, page_id, b"Rotate").unwrap_or(0);
        pages.push(PageInfo {
            index,
            object_id: object_id_string(page_id),
            media_box: media_box.to_array(),
            rotate,
        });
        let resources = page_resources(doc, page_id);
        let mut machine = Machine::new(doc, resources, index, opts);
        let contents = doc.get_page_contents(page_id);
        if contents.is_empty() {
            machine.diag("page has no content stream");
        }
        for (stream_index, content_id) in contents.iter().copied().enumerate() {
            let obj = Object::Reference(content_id);
            match stream_bytes(doc, &obj, opts.max_stream_bytes) {
                Some((_, bytes, _)) => {
                    let src = SourceCtx {
                        kind: SourceKind::PageContent,
                        object_id: Some(content_id),
                        stream_index: stream_index as u32,
                        resource_name: None,
                    };
                    machine.exec_stream(&bytes, &src);
                }
                None => machine.diag(format!(
                    "unreadable content stream {}",
                    object_id_string(content_id)
                )),
            }
        }
        machine.paint_annotations(page_id);
        diagnostics.extend(machine.diagnostics);
        glyphs.extend(machine.glyphs);
    }
    for (i, glyph) in glyphs.iter_mut().enumerate() {
        glyph.id = i as u32;
    }
    Interpreted {
        pages,
        glyphs,
        diagnostics,
    }
}

#[derive(Clone)]
struct Graphics {
    ctm: Matrix,
    fill: Color,
    stroke: Color,
    fill_alpha: f32,
    stroke_alpha: f32,
    clip: Clip,
    tc: f32,
    tw: f32,
    tz: f32,
    tl: f32,
    ts: f32,
    tr: u8,
    font_size: f32,
    font_slot: Option<usize>,
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            fill: Color::black(),
            stroke: Color::black(),
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            clip: Clip::None,
            tc: 0.0,
            tw: 0.0,
            tz: 100.0,
            tl: 0.0,
            ts: 0.0,
            tr: 0,
            font_size: 0.0,
            font_slot: None,
        }
    }
}

#[derive(Clone, Copy)]
enum Clip {
    None,
    Rect(Rect),
    Empty,
    Complex,
}

#[derive(Clone)]
enum Path {
    Empty,
    Rect(Rect),
    Complex,
}

struct SourceCtx {
    kind: SourceKind,
    object_id: Option<ObjectId>,
    stream_index: u32,
    resource_name: Option<String>,
}

struct Machine<'a> {
    doc: &'a Document,
    resources: Resources,
    page_index: u32,
    opts: InterpretOptions,
    gs: Graphics,
    gs_stack: Vec<Graphics>,
    tm: Matrix,
    tlm: Matrix,
    path: Path,
    fonts: Vec<Font>,
    font_cache: HashMap<ObjectId, usize>,
    glyphs: Vec<Glyph>,
    diagnostics: Vec<Diagnostic>,
    depth: u32,
    form_stack: Vec<ObjectId>,
    in_text: bool,
    warned_text_outside: bool,
}

impl<'a> Machine<'a> {
    fn new(
        doc: &'a Document,
        resources: Resources,
        page_index: u32,
        opts: &InterpretOptions,
    ) -> Self {
        Self {
            doc,
            resources,
            page_index,
            opts: opts.clone(),
            gs: Graphics::default(),
            gs_stack: Vec::new(),
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            path: Path::Empty,
            fonts: Vec::new(),
            font_cache: HashMap::new(),
            glyphs: Vec::new(),
            diagnostics: Vec::new(),
            depth: 0,
            form_stack: Vec::new(),
            in_text: false,
            warned_text_outside: false,
        }
    }

    fn diag(&mut self, message: impl Into<String>) {
        if self.diagnostics.len() >= DIAGNOSTIC_CAP {
            return;
        }
        if self.diagnostics.len() + 1 == DIAGNOSTIC_CAP {
            self.diagnostics.push(Diagnostic {
                page_index: Some(self.page_index),
                message: "further diagnostics suppressed".into(),
            });
            return;
        }
        self.diagnostics.push(Diagnostic {
            page_index: Some(self.page_index),
            message: message.into(),
        });
    }

    fn exec_stream(&mut self, bytes: &[u8], src: &SourceCtx) {
        let lexed = lex(bytes);
        for message in lexed.diagnostics {
            self.diag(message);
        }
        for (index, op) in lexed.ops.iter().enumerate() {
            if let Err(message) = self.dispatch(op, index as u32, src) {
                self.diag(format!("operator '{}' skipped: {message}", op.operator));
            }
        }
    }

    fn dispatch(&mut self, op: &ContentOp, index: u32, src: &SourceCtx) -> Result<(), String> {
        let range = op.start..op.end;
        match op.operator.as_str() {
            "q" => self.gs_stack.push(self.gs.clone()),
            "Q" => {
                self.gs = self
                    .gs_stack
                    .pop()
                    .ok_or_else(|| "Q without q".to_string())?;
            }
            "cm" => {
                let n = numbers(op, 6)?;
                let m = Matrix::new(n[0], n[1], n[2], n[3], n[4], n[5]);
                self.gs.ctm = m.multiply(self.gs.ctm);
            }
            "BT" => {
                self.tm = Matrix::IDENTITY;
                self.tlm = Matrix::IDENTITY;
                self.in_text = true;
            }
            "ET" => self.in_text = false,
            "Tc" => self.gs.tc = numbers(op, 1)?[0],
            "Tw" => self.gs.tw = numbers(op, 1)?[0],
            "Tz" => self.gs.tz = numbers(op, 1)?[0],
            "TL" => self.gs.tl = numbers(op, 1)?[0],
            "Tr" => self.gs.tr = numbers(op, 1)?[0] as u8,
            "Ts" => self.gs.ts = numbers(op, 1)?[0],
            "Tf" => {
                let name = name_at(op, 0)?;
                let size = number_at(op, 1)?;
                self.select_font(&name, size);
            }
            "Td" => {
                let n = numbers(op, 2)?;
                self.td(n[0], n[1]);
            }
            "TD" => {
                let n = numbers(op, 2)?;
                self.gs.tl = -n[1];
                self.td(n[0], n[1]);
            }
            "Tm" => {
                let n = numbers(op, 6)?;
                self.tm = Matrix::new(n[0], n[1], n[2], n[3], n[4], n[5]);
                self.tlm = self.tm;
            }
            "T*" => self.td(0.0, -self.gs.tl),
            "Tj" => self.show_text(&string_at(op, 0)?, index, range, src),
            "'" => {
                self.td(0.0, -self.gs.tl);
                self.show_text(&string_at(op, 0)?, index, range, src);
            }
            "\"" => {
                self.gs.tw = number_at(op, 0)?;
                self.gs.tc = number_at(op, 1)?;
                self.td(0.0, -self.gs.tl);
                self.show_text(&string_at(op, 2)?, index, range, src);
            }
            "TJ" => self.show_tj(&array_at(op, 0)?, index, range, src),
            "Do" => self.paint_xobject(&name_at(op, 0)?, index, range),
            "gs" => self.apply_ext_gstate(&name_at(op, 0)?)?,
            "g" => self.gs.fill = Color::gray(numbers(op, 1)?[0]),
            "G" => self.gs.stroke = Color::gray(numbers(op, 1)?[0]),
            "rg" => {
                let n = numbers(op, 3)?;
                self.gs.fill = Color::rgb(n[0], n[1], n[2]);
            }
            "RG" => {
                let n = numbers(op, 3)?;
                self.gs.stroke = Color::rgb(n[0], n[1], n[2]);
            }
            "k" => {
                let n = numbers(op, 4)?;
                self.gs.fill = Color::cmyk(n[0], n[1], n[2], n[3]);
            }
            "K" => {
                let n = numbers(op, 4)?;
                self.gs.stroke = Color::cmyk(n[0], n[1], n[2], n[3]);
            }
            "cs" | "CS" | "sc" | "SC" | "scn" | "SCN" => self.color_op(op)?,
            "re" => {
                let n = numbers(op, 4)?;
                let rect = Rect::new(n[0], n[1], n[0] + n[2], n[1] + n[3]);
                self.path = match self.path {
                    Path::Empty => Path::Rect(rect),
                    _ => Path::Complex,
                };
            }
            "m" | "l" | "c" | "v" | "y" | "h" => self.path = Path::Complex,
            "W" | "W*" => self.apply_clip(),
            "n" | "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => self.path = Path::Empty,
            "d0" | "d1" | "BI" | "ID" | "EI" | "BX" | "EX" | "BMC" | "BDC" | "EMC" | "MP"
            | "DP" | "w" | "J" | "j" | "M" | "d" | "ri" | "i" | "sh" => {}
            other => {
                self.diag(format!("unknown operator '{other}'"));
            }
        }
        Ok(())
    }

    fn color_op(&mut self, op: &ContentOp) -> Result<(), String> {
        let stroke = op
            .operator
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase());
        if op.operator == "cs" || op.operator == "CS" {
            return Ok(());
        }
        let nums: Vec<f32> = op
            .operands
            .iter()
            .filter_map(|o| match o {
                Operand::Number(n) => Some(*n),
                _ => None,
            })
            .collect();
        let color = match nums.as_slice() {
            [g] => Color::gray(*g),
            [r, g, b] => Color::rgb(*r, *g, *b),
            [c, m, y, k] => Color::cmyk(*c, *m, *y, *k),
            _ if nums.is_empty() => return Ok(()),
            _ => Color {
                space: "unknown".into(),
                components: nums,
            },
        };
        if stroke {
            self.gs.stroke = color;
        } else {
            self.gs.fill = color;
        }
        Ok(())
    }

    fn td(&mut self, tx: f32, ty: f32) {
        self.tlm = Matrix::translate(tx, ty).multiply(self.tlm);
        self.tm = self.tlm;
    }

    fn apply_clip(&mut self) {
        match self.path.clone() {
            Path::Empty => self.diag("clip with an empty path"),
            Path::Complex => self.gs.clip = Clip::Complex,
            Path::Rect(rect) => {
                let page = rect.transform(self.gs.ctm);
                self.gs.clip = match self.gs.clip {
                    Clip::None => Clip::Rect(page),
                    Clip::Rect(current) => match current.intersect(page) {
                        Some(inner) => Clip::Rect(inner),
                        None => Clip::Empty,
                    },
                    Clip::Empty => Clip::Empty,
                    Clip::Complex => Clip::Complex,
                };
            }
        }
    }

    fn select_font(&mut self, name: &str, size: f32) {
        self.gs.font_size = size;
        let Some(obj) = self.resources.fonts.get(name).cloned() else {
            self.diag(format!("font /{name} is not in the resource dictionary"));
            self.gs.font_slot = None;
            return;
        };
        self.select_font_object(name, &obj, size);
    }

    fn select_font_object(&mut self, name: &str, obj: &Object, size: f32) {
        self.gs.font_size = size;
        let oid = match obj {
            Object::Reference(id) => Some(*id),
            _ => None,
        };
        if let Some(id) = oid {
            if let Some(slot) = self.font_cache.get(&id).copied() {
                self.gs.font_slot = Some(slot);
                return;
            }
        }
        let doc = self.doc;
        let font = load_font(doc, name, obj);
        if let Some(id) = font.object_id {
            self.font_cache.insert(id, self.fonts.len());
        }
        self.gs.font_slot = Some(self.fonts.len());
        self.fonts.push(font);
    }

    fn apply_ext_gstate(&mut self, name: &str) -> Result<(), String> {
        let obj = self
            .resources
            .ext_gstates
            .get(name)
            .cloned()
            .ok_or_else(|| format!("ExtGState /{name} not found"))?;
        let doc = self.doc;
        let dict =
            dict_of(doc, &obj).ok_or_else(|| format!("ExtGState /{name} is not a dictionary"))?;
        if let Some(alpha) = dict.get(b"ca").ok().and_then(|o| as_f32(deref(doc, o))) {
            self.gs.fill_alpha = alpha;
        }
        if let Some(alpha) = dict.get(b"CA").ok().and_then(|o| as_f32(deref(doc, o))) {
            self.gs.stroke_alpha = alpha;
        }
        if let Some(font) = dict.get(b"Font").ok().and_then(|o| array_of(doc, o)) {
            if font.len() >= 2 {
                let size = as_f32(deref(doc, &font[1])).unwrap_or(self.gs.font_size);
                let font_obj = deref(doc, &font[0]);
                if let Some(fname) = as_name(font_obj) {
                    self.select_font(&fname, size);
                } else {
                    self.select_font_object(name, &font[0], size);
                }
            }
        }
        Ok(())
    }

    fn show_tj(
        &mut self,
        items: &[Operand],
        index: u32,
        range: std::ops::Range<usize>,
        src: &SourceCtx,
    ) {
        for item in items {
            match item {
                Operand::Number(n) => {
                    // The number sits between glyphs. Fold it into the previous
                    // glyph's advance so origin[i+1] = origin[i] + advance[i].
                    let delta = self.adjust(*n);
                    if let Some(prev) = self.glyphs.last_mut() {
                        if prev.source.operator_index == index && prev.page_index == self.page_index
                        {
                            prev.advance[0] += delta[0];
                            prev.advance[1] += delta[1];
                        }
                    }
                }
                Operand::String(bytes) => self.show_text(bytes, index, range.clone(), src),
                _ => self.diag("TJ array entry is neither a string nor a number"),
            }
        }
    }

    fn show_text(
        &mut self,
        bytes: &[u8],
        index: u32,
        range: std::ops::Range<usize>,
        src: &SourceCtx,
    ) {
        if !self.in_text && !self.warned_text_outside {
            self.warned_text_outside = true;
            self.diag("text showing operator outside BT/ET; recording it anyway");
        }
        let Some(slot) = self.gs.font_slot else {
            self.show_without_font(bytes, index, range, src);
            return;
        };
        let decoded = self.fonts[slot].decode(bytes);
        let font_matrix = self.fonts[slot].font_matrix;
        let type3_resources = self.fonts[slot].type3.as_ref().map(|t| t.resources.clone());
        let procs: Vec<Option<Vec<u8>>> = decoded
            .iter()
            .map(|glyph| {
                self.fonts[slot]
                    .type3
                    .as_ref()
                    .and_then(|t| t.procs.get(&glyph.code).cloned())
            })
            .collect();
        for (glyph, proc) in decoded.into_iter().zip(procs) {
            let glyph_index = self.glyphs.len();
            self.emit(slot, &glyph, index, range.clone(), src);
            if let Some(proc_bytes) = proc {
                self.run_type3(font_matrix, type3_resources.clone(), &proc_bytes, src);
            }
            let advance = self.advance_glyph(&glyph, 0.0);
            if let Some(record) = self.glyphs.get_mut(glyph_index) {
                record.advance = advance;
            }
        }
        if self.gs.tr >= 4 {
            // Glyph outlines were added to the clip. The exact outline is not tracked.
            self.gs.clip = Clip::Complex;
        }
    }

    fn show_without_font(
        &mut self,
        bytes: &[u8],
        index: u32,
        range: std::ops::Range<usize>,
        src: &SourceCtx,
    ) {
        for (offset, byte) in bytes.iter().copied().enumerate() {
            let fake = DecodedGlyph {
                code: byte as u32,
                code_bytes: vec![byte],
                unicode: None,
                gid: None,
                w0: 0.5,
                vertical_metrics: None,
            };
            let glyph_index = self.glyphs.len();
            self.emit_raw(&fake, "unknown", "unknown", None, index, range.clone(), src);
            let _ = offset;
            let advance = self.advance_glyph(&fake, 0.0);
            if let Some(record) = self.glyphs.get_mut(glyph_index) {
                record.advance = advance;
            }
        }
    }

    fn emit(
        &mut self,
        slot: usize,
        glyph: &DecodedGlyph,
        index: u32,
        range: std::ops::Range<usize>,
        src: &SourceCtx,
    ) {
        let resource = self.fonts[slot].resource_name.clone();
        let base = self.fonts[slot].base_name.clone();
        let object = self.fonts[slot].object_id_string();
        self.emit_raw(glyph, &resource, &base, object, index, range, src);
    }

    fn emit_raw(
        &mut self,
        glyph: &DecodedGlyph,
        resource: &str,
        base: &str,
        object: Option<String>,
        index: u32,
        range: std::ops::Range<usize>,
        src: &SourceCtx,
    ) {
        let trm = self.rendering_matrix(glyph);
        // FrameMaker sets Tf to 1 and scales the text matrix. The size that
        // layout sees has to be the user-space height, not the Tf number.
        let visual = (trm.c * trm.c + trm.d * trm.d).sqrt();
        let font_size = if visual > 0.01 {
            visual
        } else {
            self.gs.font_size
        };
        let width = glyph.w0.max(0.0);
        let corners = [
            trm.transform_point(0.0, -0.2),
            trm.transform_point(width, -0.2),
            trm.transform_point(0.0, 0.8),
            trm.transform_point(width, 0.8),
        ];
        let bbox = Rect::from_points(&corners);
        let (clipped, clip_uncertain) = match self.gs.clip {
            Clip::None => (false, false),
            Clip::Complex => (false, true),
            Clip::Empty => (true, false),
            Clip::Rect(rect) => (!rect.intersects(bbox), false),
        };
        let invisible = self.gs.tr == 3
            || self.gs.tr == 7
            || (self.gs.fill_alpha <= 0.0 && matches!(self.gs.tr, 0 | 4));
        let unicode = glyph.unicode.clone().unwrap_or_default();
        let unmapped = glyph.unicode.is_none();
        self.glyphs.push(Glyph {
            id: 0,
            page_index: self.page_index,
            unicode,
            unmapped,
            char_code: glyph.code_bytes.clone(),
            gid: glyph.gid,
            font_resource: resource.into(),
            font_name: base.into(),
            font_object: object,
            font_size,
            matrix: trm.to_array(),
            bbox: bbox.to_array(),
            advance: [0.0, 0.0],
            fill_color: self.gs.fill.clone(),
            stroke_color: self.gs.stroke.clone(),
            render_mode: self.gs.tr,
            invisible,
            clipped,
            clip_uncertain,
            vertical: glyph.vertical_metrics.is_some(),
            disposition: Disposition::Pending,
            source: GlyphSource {
                kind: src.kind.clone(),
                object_id: src.object_id.map(object_id_string),
                stream_index: src.stream_index,
                operator_index: index,
                byte_start: range.start,
                byte_end: range.end,
                resource_name: src.resource_name.clone(),
            },
        });
    }

    fn rendering_matrix(&self, glyph: &DecodedGlyph) -> Matrix {
        let th = self.gs.tz / 100.0;
        let fs = self.gs.font_size;
        let mut text = Matrix::new(fs * th, 0.0, 0.0, fs, 0.0, self.gs.ts);
        if let Some(v) = glyph.vertical_metrics {
            text = Matrix::translate(v.vx, v.vy).multiply(text);
        }
        text.multiply(self.tm).multiply(self.gs.ctm)
    }

    fn advance_glyph(&mut self, glyph: &DecodedGlyph, tj: f32) -> [f32; 2] {
        let fs = self.gs.font_size;
        let th = self.gs.tz / 100.0;
        let spacing = self.gs.tc + if is_space(glyph) { self.gs.tw } else { 0.0 };
        let (tx, ty) = if let Some(v) = glyph.vertical_metrics {
            (0.0, (v.w1 - tj / 1000.0) * fs + spacing)
        } else {
            (((glyph.w0 - tj / 1000.0) * fs + spacing) * th, 0.0)
        };
        let before = self.tm.multiply(self.gs.ctm);
        self.tm = Matrix::translate(tx, ty).multiply(self.tm);
        let after = self.tm.multiply(self.gs.ctm);
        [after.e - before.e, after.f - before.f]
    }

    fn adjust(&mut self, amount: f32) -> [f32; 2] {
        let fs = self.gs.font_size;
        let th = self.gs.tz / 100.0;
        let vertical = self
            .gs
            .font_slot
            .and_then(|slot| self.fonts.get(slot))
            .is_some_and(|font| font.vertical);
        let (tx, ty) = if vertical {
            (0.0, -amount / 1000.0 * fs)
        } else {
            (-amount / 1000.0 * fs * th, 0.0)
        };
        let before = self.tm.multiply(self.gs.ctm);
        self.tm = Matrix::translate(tx, ty).multiply(self.tm);
        let after = self.tm.multiply(self.gs.ctm);
        [after.e - before.e, after.f - before.f]
    }

    fn run_type3(
        &mut self,
        font_matrix: Matrix,
        resources: Option<Resources>,
        bytes: &[u8],
        parent: &SourceCtx,
    ) {
        if self.depth >= self.opts.max_depth {
            self.diag("Type3 charproc recursion limit");
            return;
        }
        self.depth += 1;
        let trm = {
            let th = self.gs.tz / 100.0;
            let fs = self.gs.font_size;
            Matrix::new(fs * th, 0.0, 0.0, fs, 0.0, self.gs.ts)
                .multiply(self.tm)
                .multiply(self.gs.ctm)
        };
        let new_ctm = font_matrix.multiply(trm);
        let saved_resources = self.resources.clone();
        let saved_tm = self.tm;
        let saved_tlm = self.tlm;
        let saved_path = self.path.clone();
        let saved_text = self.in_text;
        self.gs_stack.push(self.gs.clone());
        self.gs.ctm = new_ctm;
        if let Some(resources) = resources {
            self.resources.merge_override(resources);
        }
        self.tm = Matrix::IDENTITY;
        self.tlm = Matrix::IDENTITY;
        self.path = Path::Empty;
        self.in_text = false;
        let src = SourceCtx {
            kind: SourceKind::Type3CharProc,
            object_id: parent.object_id,
            stream_index: parent.stream_index,
            resource_name: parent.resource_name.clone(),
        };
        self.exec_stream(bytes, &src);
        self.gs = self.gs_stack.pop().unwrap_or_default();
        self.resources = saved_resources;
        self.tm = saved_tm;
        self.tlm = saved_tlm;
        self.path = saved_path;
        self.in_text = saved_text;
        self.depth -= 1;
    }

    fn paint_xobject(&mut self, name: &str, _index: u32, _range: std::ops::Range<usize>) {
        let Some(obj) = self.resources.xobjects.get(name).cloned() else {
            self.diag(format!("XObject /{name} not found"));
            return;
        };
        let doc = self.doc;
        let limit = self.opts.max_stream_bytes;
        let Some((oid, bytes, dict)) = stream_bytes(doc, &obj, limit) else {
            self.diag(format!("XObject /{name} is not a stream"));
            return;
        };
        let subtype = dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| as_name(deref(doc, o)));
        if subtype.as_deref() == Some("Image")
            || (subtype.is_none() && dict.has(b"Width") && dict.has(b"Height"))
        {
            return;
        }
        if let Some(kind) = subtype.as_deref() {
            if kind != "Form" {
                self.diag(format!("XObject /{name} subtype /{kind} ignored"));
                return;
            }
        }
        self.paint_form(oid, &bytes, &dict, name, SourceKind::FormXObject, None);
    }

    fn paint_form(
        &mut self,
        oid: Option<ObjectId>,
        bytes: &[u8],
        dict: &lopdf::Dictionary,
        name: &str,
        kind: SourceKind,
        replace_ctm: Option<Matrix>,
    ) {
        if self.depth >= self.opts.max_depth {
            self.diag(format!("recursion limit while painting /{name}"));
            return;
        }
        if let Some(id) = oid {
            if self.form_stack.contains(&id) {
                self.diag(format!("cycle in form /{name}"));
                return;
            }
            self.form_stack.push(id);
        }
        self.depth += 1;
        let doc = self.doc;
        let saved_resources = self.resources.clone();
        let saved_tm = self.tm;
        let saved_tlm = self.tlm;
        let saved_path = self.path.clone();
        let saved_text = self.in_text;
        if let Ok(res_obj) = dict.get(b"Resources") {
            if let Some(res_dict) = dict_of(doc, res_obj) {
                let child = Resources::from_dict(doc, res_dict);
                self.resources.merge_override(child);
            }
        }
        self.gs_stack.push(self.gs.clone());
        if let Some(ctm) = replace_ctm {
            self.gs.ctm = ctm;
        } else if let Some(matrix) = matrix_from(dict) {
            self.gs.ctm = matrix.multiply(self.gs.ctm);
        }
        if let Some(bbox) = rect_from(dict, b"BBox") {
            let page = bbox.transform(self.gs.ctm);
            self.gs.clip = match self.gs.clip {
                Clip::None => Clip::Rect(page),
                Clip::Rect(current) => current
                    .intersect(page)
                    .map(Clip::Rect)
                    .unwrap_or(Clip::Empty),
                Clip::Empty => Clip::Empty,
                Clip::Complex => Clip::Complex,
            };
        }
        self.tm = Matrix::IDENTITY;
        self.tlm = Matrix::IDENTITY;
        self.path = Path::Empty;
        self.in_text = false;
        let src = SourceCtx {
            kind,
            object_id: oid,
            stream_index: 0,
            resource_name: Some(name.to_string()),
        };
        self.exec_stream(bytes, &src);
        self.gs = self.gs_stack.pop().unwrap_or_default();
        self.resources = saved_resources;
        self.tm = saved_tm;
        self.tlm = saved_tlm;
        self.path = saved_path;
        self.in_text = saved_text;
        self.depth -= 1;
        if oid.is_some() {
            self.form_stack.pop();
        }
    }

    fn paint_annotations(&mut self, page_id: ObjectId) {
        let doc = self.doc;
        let Ok(page) = doc.get_dictionary(page_id) else {
            return;
        };
        let Ok(annots) = page.get(b"Annots") else {
            return;
        };
        let Some(annots) = array_of(doc, annots) else {
            return;
        };
        let annots = annots.clone();
        for (index, annot) in annots.iter().enumerate() {
            let oid = match annot {
                Object::Reference(id) => Some(*id),
                _ => None,
            };
            let Some(dict) = dict_of(doc, annot).cloned() else {
                self.diag(format!("annotation {index} is not a dictionary"));
                continue;
            };
            self.paint_one_annotation(index, oid, &dict);
        }
    }

    fn paint_one_annotation(
        &mut self,
        index: usize,
        oid: Option<ObjectId>,
        dict: &lopdf::Dictionary,
    ) {
        let doc = self.doc;
        let Some(ap) = dict.get(b"AP").ok().and_then(|o| dict_of(doc, o)) else {
            return;
        };
        let Some(normal) = ap.get(b"N").ok() else {
            return;
        };
        let normal = deref(doc, normal);
        let stream_obj = if normal.as_stream().is_ok() {
            normal.clone()
        } else if let Ok(states) = normal.as_dict() {
            let selected = dict
                .get(b"AS")
                .ok()
                .and_then(|o| as_name(deref(doc, o)))
                .and_then(|name| states.get(name.as_bytes()).ok().cloned());
            match selected.or_else(|| states.iter().next().map(|(_, v)| v.clone())) {
                Some(obj) => obj,
                None => return,
            }
        } else {
            return;
        };
        let limit = self.opts.max_stream_bytes;
        let Some((stream_id, bytes, stream_dict)) = stream_bytes(doc, &stream_obj, limit) else {
            self.diag(format!("annotation {index} appearance is unreadable"));
            return;
        };
        let form_matrix = matrix_from(&stream_dict).unwrap_or(Matrix::IDENTITY);
        let bbox = rect_from(&stream_dict, b"BBox").unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
        let rect = rect_from(dict, b"Rect");
        let ctm = match rect {
            Some(rect) => appearance_matrix(rect, bbox, form_matrix),
            None => form_matrix,
        };
        let name = format!("Annot{index}");
        self.paint_form(
            stream_id.or(oid),
            &bytes,
            &stream_dict,
            &name,
            SourceKind::AnnotationAppearance,
            Some(ctm),
        );
    }
}

fn is_space(glyph: &DecodedGlyph) -> bool {
    glyph.code_bytes.as_slice() == [0x20] || (glyph.code == 32 && glyph.code_bytes.len() == 1)
}

fn numbers(op: &ContentOp, n: usize) -> Result<Vec<f32>, String> {
    if op.operands.len() < n {
        return Err(format!(
            "expected {n} operands, found {}",
            op.operands.len()
        ));
    }
    let start = op.operands.len() - n;
    let mut out = Vec::with_capacity(n);
    for operand in &op.operands[start..] {
        match operand {
            Operand::Number(v) => out.push(*v),
            _ => return Err("expected a number operand".into()),
        }
    }
    Ok(out)
}

fn number_at(op: &ContentOp, index: usize) -> Result<f32, String> {
    match op.operands.get(index) {
        Some(Operand::Number(v)) => Ok(*v),
        _ => Err(format!("operand {index} is not a number")),
    }
}

fn name_at(op: &ContentOp, index: usize) -> Result<String, String> {
    match op.operands.get(index) {
        Some(Operand::Name(v)) => Ok(v.clone()),
        _ => Err(format!("operand {index} is not a name")),
    }
}

fn string_at(op: &ContentOp, index: usize) -> Result<Vec<u8>, String> {
    match op.operands.get(index) {
        Some(Operand::String(v)) => Ok(v.clone()),
        _ => Err(format!("operand {index} is not a string")),
    }
}

fn array_at(op: &ContentOp, index: usize) -> Result<Vec<Operand>, String> {
    match op.operands.get(index) {
        Some(Operand::Array(v)) => Ok(v.clone()),
        _ => Err(format!("operand {index} is not an array")),
    }
}

fn matrix_from(dict: &lopdf::Dictionary) -> Option<Matrix> {
    let arr = dict.get(b"Matrix").ok()?.as_array().ok()?;
    if arr.len() < 6 {
        return None;
    }
    let n = |i: usize| as_f32(&arr[i]).unwrap_or(0.0);
    Some(Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5)))
}

fn rect_from(dict: &lopdf::Dictionary, key: &[u8]) -> Option<Rect> {
    let arr = dict.get(key).ok()?.as_array().ok()?;
    if arr.len() < 4 {
        return None;
    }
    let n = |i: usize| as_f32(&arr[i]).unwrap_or(0.0);
    Some(Rect::new(n(0), n(1), n(2), n(3)))
}

fn appearance_matrix(rect: Rect, bbox: Rect, form_matrix: Matrix) -> Matrix {
    let bw = bbox.width();
    let bh = bbox.height();
    let sx = if bw.abs() < 1e-6 {
        1.0
    } else {
        rect.width() / bw
    };
    let sy = if bh.abs() < 1e-6 {
        1.0
    } else {
        rect.height() / bh
    };
    form_matrix
        .multiply(Matrix::translate(-bbox.x0, -bbox.y0))
        .multiply(Matrix::scale(sx, sy))
        .multiply(Matrix::translate(rect.x0, rect.y0))
}

fn page_resources(doc: &Document, page_id: ObjectId) -> Resources {
    let mut chain = Vec::new();
    let mut current = Some(page_id);
    let mut seen = HashSet::new();
    while let Some(id) = current {
        if !seen.insert(id) || chain.len() > 32 {
            break;
        }
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
        chain.push(id);
        current = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    }
    let mut resources = Resources::default();
    for id in chain.iter().rev() {
        let Ok(dict) = doc.get_dictionary(*id) else {
            continue;
        };
        if let Ok(res) = dict.get(b"Resources") {
            if let Some(res_dict) = dict_of(doc, res) {
                resources.merge_override(Resources::from_dict(doc, res_dict));
            }
        }
    }
    resources
}

fn inherited_rect(doc: &Document, mut id: ObjectId, key: &[u8]) -> Option<Rect> {
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(id) {
            return None;
        }
        let dict = doc.get_dictionary(id).ok()?;
        if let Some(rect) = dict.get(key).ok().and_then(|o| array_of(doc, o)) {
            if rect.len() >= 4 {
                let n = |i: usize| as_f32(deref(doc, &rect[i])).unwrap_or(0.0);
                return Some(Rect::new(n(0), n(1), n(2), n(3)));
            }
        }
        id = dict
            .get(b"Parent")
            .ok()
            .and_then(|o| o.as_reference().ok())?;
    }
}

fn inherited_int(doc: &Document, mut id: ObjectId, key: &[u8]) -> Option<i32> {
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(id) {
            return None;
        }
        let dict = doc.get_dictionary(id).ok()?;
        if let Some(v) = dict.get(key).ok().and_then(|o| deref(doc, o).as_i64().ok()) {
            return Some(v as i32);
        }
        id = dict
            .get(b"Parent")
            .ok()
            .and_then(|o| o.as_reference().ok())?;
    }
}
