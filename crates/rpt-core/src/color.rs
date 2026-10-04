use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub space: String,
    pub components: Vec<f32>,
}

impl Color {
    pub fn black() -> Self {
        Self {
            space: "DeviceGray".into(),
            components: vec![0.0],
        }
    }

    pub fn gray(g: f32) -> Self {
        Self {
            space: "DeviceGray".into(),
            components: vec![g],
        }
    }

    pub fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self {
            space: "DeviceRGB".into(),
            components: vec![r, g, b],
        }
    }

    pub fn cmyk(c: f32, m: f32, y: f32, k: f32) -> Self {
        Self {
            space: "DeviceCMYK".into(),
            components: vec![c, m, y, k],
        }
    }
}

impl Default for Color {
    fn default() -> Self {
        Self::black()
    }
}
