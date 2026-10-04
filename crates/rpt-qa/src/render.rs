//! Render original and comparison PDFs with Poppler and score the pages.
//!
//! PDFium is not required. `pdftoppm` is the dev-tool renderer.

use std::path::{Path, PathBuf};
use std::process::Command;

use image::GrayImage;
use rpt_core::{Extraction, PageInfo};
use serde::Serialize;

use crate::ssim::{exact_ratio, mean_ssim, nontext_ssim};

#[derive(Clone, Debug, Serialize)]
pub struct PageRender {
    pub page: u32,
    pub width: u32,
    pub height: u32,
    pub exact_ratio: f32,
    pub ssim: Option<f32>,
    pub nontext_ssim: Option<f32>,
    pub nontext_note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RenderReport {
    pub tool: String,
    pub available: bool,
    pub pages: Vec<PageRender>,
    pub note: String,
}

pub fn compare_renders(
    original: &Path,
    other: &Path,
    extraction: &Extraction,
    page_count: u32,
    work: &Path,
) -> RenderReport {
    if Command::new("pdftoppm").arg("-v").output().is_err() {
        return RenderReport {
            tool: "pdftoppm".into(),
            available: false,
            pages: Vec::new(),
            note: "pdftoppm is not installed; render-diff skipped".into(),
        };
    }
    let left_dir = work.join("left");
    let right_dir = work.join("right");
    let _ = std::fs::create_dir_all(&left_dir);
    let _ = std::fs::create_dir_all(&right_dir);
    if let Err(err) = rasterize(original, &left_dir, page_count) {
        return RenderReport {
            tool: "pdftoppm".into(),
            available: true,
            pages: Vec::new(),
            note: format!("failed to render original: {err}"),
        };
    }
    if let Err(err) = rasterize(other, &right_dir, page_count) {
        return RenderReport {
            tool: "pdftoppm".into(),
            available: true,
            pages: Vec::new(),
            note: format!("failed to render comparison pdf: {err}"),
        };
    }
    let mut pages = Vec::new();
    for page in 1..=page_count {
        let left = page_png(&left_dir, page);
        let right = page_png(&right_dir, page);
        let (Some(left), Some(right)) = (left, right) else {
            pages.push(PageRender {
                page,
                width: 0,
                height: 0,
                exact_ratio: 0.0,
                ssim: None,
                nontext_ssim: None,
                nontext_note: "missing raster".into(),
            });
            continue;
        };
        pages.push(score_page(page, &left, &right, extraction));
    }
    let _ = std::fs::remove_dir_all(work);
    RenderReport {
        tool: "pdftoppm".into(),
        available: true,
        pages,
        note: "72 dpi grayscale; non-text SSIM drops blocks that are mostly glyph boxes".into(),
    }
}

fn rasterize(pdf: &Path, dir: &Path, pages: u32) -> Result<(), String> {
    let status = Command::new("pdftoppm")
        .args([
            "-png",
            "-r",
            "72",
            "-f",
            "1",
            "-l",
            &pages.to_string(),
            pdf.to_str().ok_or("path is not utf-8")?,
            dir.join("page").to_str().ok_or("path is not utf-8")?,
        ])
        .status()
        .map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pdftoppm exited with {status}"))
    }
}

fn page_png(dir: &Path, page: u32) -> Option<PathBuf> {
    // pdftoppm zero-pads using the document page count, so a 1016-page file
    // writes `page-0001.png` even when only the first two pages are rendered.
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name
            .strip_prefix("page-")
            .and_then(|s| s.strip_suffix(".png"))
        else {
            continue;
        };
        if rest.parse::<u32>().ok() == Some(page) {
            return Some(entry.path());
        }
    }
    None
}

fn score_page(page: u32, left: &Path, right: &Path, extraction: &Extraction) -> PageRender {
    let left_img = match image::open(left).map(|img| img.to_luma8()) {
        Ok(img) => img,
        Err(err) => {
            return PageRender {
                page,
                width: 0,
                height: 0,
                exact_ratio: 0.0,
                ssim: None,
                nontext_ssim: None,
                nontext_note: format!("decode failed: {err}"),
            }
        }
    };
    let right_img = match image::open(right).map(|img| img.to_luma8()) {
        Ok(img) => img,
        Err(err) => {
            return PageRender {
                page,
                width: 0,
                height: 0,
                exact_ratio: 0.0,
                ssim: None,
                nontext_ssim: None,
                nontext_note: format!("decode failed: {err}"),
            }
        }
    };
    if left_img.dimensions() != right_img.dimensions() {
        return PageRender {
            page,
            width: left_img.width(),
            height: left_img.height(),
            exact_ratio: 0.0,
            ssim: None,
            nontext_ssim: None,
            nontext_note: format!(
                "size mismatch {}x{} vs {}x{}",
                left_img.width(),
                left_img.height(),
                right_img.width(),
                right_img.height()
            ),
        };
    }
    let (width, height) = left_img.dimensions();
    let left_px = luma(&left_img);
    let right_px = luma(&right_img);
    let width = width as usize;
    let height = height as usize;
    let info = extraction.pages.iter().find(|item| item.index + 1 == page);
    let (nontext, note) = match info {
        Some(info) if info.rotate % 360 == 0 => {
            let mask = text_mask(extraction, info, width, height);
            (
                nontext_ssim(&left_px, &right_px, width, height, &mask),
                "glyph boxes masked".into(),
            )
        }
        Some(info) => (
            None,
            format!("page rotate {} skips the non-text mask", info.rotate),
        ),
        None => (None, "no extracted page info".into()),
    };
    PageRender {
        page,
        width: width as u32,
        height: height as u32,
        exact_ratio: exact_ratio(&left_px, &right_px),
        ssim: mean_ssim(&left_px, &right_px, width, height),
        nontext_ssim: nontext,
        nontext_note: note,
    }
}

fn luma(image: &GrayImage) -> Vec<f32> {
    image.pixels().map(|pixel| pixel[0] as f32).collect()
}

fn text_mask(extraction: &Extraction, page: &PageInfo, width: usize, height: usize) -> Vec<bool> {
    let mut mask = vec![false; width * height];
    let box_ = page.media_box;
    let page_w = (box_[2] - box_[0]).abs().max(1.0);
    let page_h = (box_[3] - box_[1]).abs().max(1.0);
    let scale_x = width as f32 / page_w;
    let scale_y = height as f32 / page_h;
    for glyph in extraction
        .glyphs
        .iter()
        .filter(|glyph| glyph.page_index == page.index)
    {
        let x0 = glyph.bbox[0].min(glyph.bbox[2]);
        let x1 = glyph.bbox[0].max(glyph.bbox[2]);
        let y0 = glyph.bbox[1].min(glyph.bbox[3]);
        let y1 = glyph.bbox[1].max(glyph.bbox[3]);
        let px0 = ((x0 - box_[0]) * scale_x).floor() as i32 - 1;
        let px1 = ((x1 - box_[0]) * scale_x).ceil() as i32 + 1;
        let py0 = (height as f32 - (y1 - box_[1]) * scale_y).floor() as i32 - 1;
        let py1 = (height as f32 - (y0 - box_[1]) * scale_y).ceil() as i32 + 1;
        for y in py0.max(0)..py1.min(height as i32) {
            for x in px0.max(0)..px1.min(width as i32) {
                mask[y as usize * width + x as usize] = true;
            }
        }
    }
    mask
}
