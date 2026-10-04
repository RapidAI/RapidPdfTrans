//! Horizontal `/W` `/DW` and vertical `/W2` `/DW2` width maps.
//!
//! Widths are stored in glyph-space units (1000 = 1 em for a standard font
//! matrix). The caller applies `/FontMatrix`.

use std::collections::HashMap;

use lopdf::{Document, Object};

use crate::pdfutil::{array_of, as_f32, deref};

#[derive(Clone, Debug)]
pub struct WidthMap {
    default: f32,
    exact: HashMap<u32, f32>,
    ranges: Vec<(u32, u32, f32)>,
}

impl WidthMap {
    pub fn constant(default: f32) -> Self {
        Self {
            default,
            exact: HashMap::new(),
            ranges: Vec::new(),
        }
    }

    pub fn get(&self, cid: u32) -> f32 {
        if let Some(w) = self.exact.get(&cid) {
            return *w;
        }
        for (start, end, w) in &self.ranges {
            if cid >= *start && cid <= *end {
                return *w;
            }
        }
        self.default
    }

    /// Simple-font `/Widths` from `first` through `last`, plus `/MissingWidth`.
    pub fn from_simple(first: u32, widths: &[f32], missing: f32) -> Self {
        let mut exact = HashMap::new();
        for (i, w) in widths.iter().enumerate() {
            exact.insert(first + i as u32, *w);
        }
        Self {
            default: missing,
            exact,
            ranges: Vec::new(),
        }
    }

    pub fn parse_w(doc: &Document, array: &Object, default: f32) -> Self {
        let mut map = Self::constant(default);
        let Some(items) = array_of(doc, array) else {
            return map;
        };
        let mut i = 0;
        while i < items.len() {
            let Some(start) = as_f32(deref(doc, &items[i])) else {
                i += 1;
                continue;
            };
            let start = start as u32;
            if i + 1 >= items.len() {
                break;
            }
            let next = deref(doc, &items[i + 1]);
            if let Object::Array(ws) = next {
                for (k, wobj) in ws.iter().enumerate() {
                    if let Some(w) = as_f32(deref(doc, wobj)) {
                        map.exact.insert(start + k as u32, w);
                    }
                }
                i += 2;
            } else if i + 2 < items.len() {
                if let (Some(end), Some(w)) = (
                    as_f32(deref(doc, &items[i + 1])),
                    as_f32(deref(doc, &items[i + 2])),
                ) {
                    map.ranges.push((start, end as u32, w));
                    i += 3;
                } else {
                    i += 1;
                }
            } else {
                break;
            }
        }
        map
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VMetrics {
    pub w1: f32,
    pub vx: f32,
    pub vy: f32,
}

#[derive(Clone, Debug)]
pub struct VerticalMap {
    default_w1: f32,
    default_vy: f32,
    exact: HashMap<u32, VMetrics>,
    ranges: Vec<(u32, u32, VMetrics)>,
}

impl Default for VerticalMap {
    fn default() -> Self {
        // PDF default DW2 is [880 -1000]. vx defaults to half the horizontal width.
        Self {
            default_w1: 880.0,
            default_vy: -1000.0,
            exact: HashMap::new(),
            ranges: Vec::new(),
        }
    }
}

impl VerticalMap {
    pub fn get(&self, cid: u32, w0: f32) -> VMetrics {
        if let Some(v) = self.exact.get(&cid) {
            return *v;
        }
        for (start, end, v) in &self.ranges {
            if cid >= *start && cid <= *end {
                return *v;
            }
        }
        VMetrics {
            w1: self.default_w1,
            vx: w0 / 2.0,
            vy: self.default_vy,
        }
    }

    pub fn parse(doc: &Document, dw2: Option<&Object>, w2: Option<&Object>) -> Self {
        let mut map = VerticalMap::default();
        if let Some(obj) = dw2 {
            if let Some(arr) = array_of(doc, obj) {
                if let Some(w1) = arr.first().and_then(|o| as_f32(deref(doc, o))) {
                    map.default_w1 = w1;
                }
                if let Some(vy) = arr.get(1).and_then(|o| as_f32(deref(doc, o))) {
                    map.default_vy = vy;
                }
            }
        }
        let Some(obj) = w2 else {
            return map;
        };
        let Some(items) = array_of(doc, obj) else {
            return map;
        };
        let mut i = 0;
        while i < items.len() {
            let Some(start_f) = as_f32(deref(doc, &items[i])) else {
                i += 1;
                continue;
            };
            let start = start_f as u32;
            if i + 1 >= items.len() {
                break;
            }
            let next = deref(doc, &items[i + 1]);
            if let Object::Array(ws) = next {
                let mut k = 0;
                let mut cid = start;
                while k + 2 < ws.len() {
                    let w1 = as_f32(deref(doc, &ws[k])).unwrap_or(map.default_w1);
                    let vx = as_f32(deref(doc, &ws[k + 1])).unwrap_or(0.0);
                    let vy = as_f32(deref(doc, &ws[k + 2])).unwrap_or(map.default_vy);
                    map.exact.insert(cid, VMetrics { w1, vx, vy });
                    cid += 1;
                    k += 3;
                }
                i += 2;
            } else if i + 4 < items.len() {
                let end = as_f32(deref(doc, &items[i + 1])).map(|v| v as u32);
                let w1 = as_f32(deref(doc, &items[i + 2]));
                let vx = as_f32(deref(doc, &items[i + 3]));
                let vy = as_f32(deref(doc, &items[i + 4]));
                if let (Some(end), Some(w1), Some(vx), Some(vy)) = (end, w1, vx, vy) {
                    map.ranges.push((start, end, VMetrics { w1, vx, vy }));
                    i += 5;
                } else {
                    i += 1;
                }
            } else {
                break;
            }
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn w_array_both_forms() {
        let doc = Document::new();
        let array = Object::Array(vec![
            Object::Integer(1),
            Object::Array(vec![Object::Integer(600), Object::Integer(400)]),
            Object::Integer(10),
            Object::Integer(12),
            Object::Integer(500),
        ]);
        let map = WidthMap::parse_w(&doc, &array, 1000.0);
        assert_eq!(map.get(1), 600.0);
        assert_eq!(map.get(2), 400.0);
        assert_eq!(map.get(11), 500.0);
        assert_eq!(map.get(99), 1000.0);
    }
}
