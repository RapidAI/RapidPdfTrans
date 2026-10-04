//! PDF transformation matrices and rectangles.
//!
//! Matrices use the PDF convention `[a b c d e f]`, row vectors on the left:
//! `[x' y' 1] = [x y 1] × M`. User space has its origin at the bottom left
//! and y increases upward. Page `/Rotate` is not applied by the interpreter.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Matrix {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub fn translate(tx: f32, ty: f32) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// `self × rhs`. `self` is applied to the point first.
    pub fn multiply(self, rhs: Matrix) -> Matrix {
        Matrix {
            a: self.a * rhs.a + self.b * rhs.c,
            b: self.a * rhs.b + self.b * rhs.d,
            c: self.c * rhs.a + self.d * rhs.c,
            d: self.c * rhs.b + self.d * rhs.d,
            e: self.e * rhs.a + self.f * rhs.c + rhs.e,
            f: self.e * rhs.b + self.f * rhs.d + rhs.f,
        }
    }

    pub fn transform_point(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    pub fn transform_vector(self, x: f32, y: f32) -> (f32, f32) {
        (self.a * x + self.c * y, self.b * x + self.d * y)
    }

    pub fn to_array(self) -> [f32; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn from_points(pts: &[(f32, f32)]) -> Self {
        let mut x0 = f32::MAX;
        let mut y0 = f32::MAX;
        let mut x1 = f32::MIN;
        let mut y1 = f32::MIN;
        for (x, y) in pts {
            x0 = x0.min(*x);
            y0 = y0.min(*y);
            x1 = x1.max(*x);
            y1 = y1.max(*y);
        }
        if pts.is_empty() {
            return Self::new(0.0, 0.0, 0.0, 0.0);
        }
        Self { x0, y0, x1, y1 }
    }

    pub fn to_array(self) -> [f32; 4] {
        [self.x0, self.y0, self.x1, self.y1]
    }

    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }

    pub fn intersects(self, other: Rect) -> bool {
        const EPS: f32 = 0.01;
        self.x1 >= other.x0 - EPS
            && other.x1 >= self.x0 - EPS
            && self.y1 >= other.y0 - EPS
            && other.y1 >= self.y0 - EPS
    }

    pub fn intersect(self, other: Rect) -> Option<Self> {
        let r = Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        if r.x0 <= r.x1 && r.y0 <= r.y1 {
            Some(r)
        } else {
            None
        }
    }

    /// Axis-aligned bounding box of this rectangle transformed by `m`.
    pub fn transform(self, m: Matrix) -> Self {
        Self::from_points(&[
            m.transform_point(self.x0, self.y0),
            m.transform_point(self.x1, self.y0),
            m.transform_point(self.x0, self.y1),
            m.transform_point(self.x1, self.y1),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cm_translation_then_scale() {
        // Point is scaled by 2 about the origin, then translated by (10, 20).
        // CTM' = Scale × Translate means scale is applied first.
        let m = Matrix::scale(2.0, 2.0).multiply(Matrix::translate(10.0, 20.0));
        let (x, y) = m.transform_point(3.0, 4.0);
        assert!((x - 16.0).abs() < 1e-4, "{x}");
        assert!((y - 28.0).abs() < 1e-4, "{y}");
    }
}
