//! Block SSIM on 8-bit grayscale images.
//!
//! `text_mask` marks pixels that belong to extracted glyph boxes. Non-text
//! SSIM ignores blocks that are mostly those pixels, so a later rewrite can
//! change glyphs without hiding a moved figure or rule.

const C1: f32 = (0.01 * 255.0) * (0.01 * 255.0);
const C2: f32 = (0.03 * 255.0) * (0.03 * 255.0);

pub fn exact_ratio(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let same = a
        .iter()
        .zip(b)
        .filter(|(x, y)| (*x - *y).abs() < 0.5)
        .count();
    same as f32 / a.len() as f32
}

pub fn mean_ssim(a: &[f32], b: &[f32], width: usize, height: usize) -> Option<f32> {
    block_mean(a, b, width, height, None)
}

pub fn nontext_ssim(
    a: &[f32],
    b: &[f32],
    width: usize,
    height: usize,
    text_mask: &[bool],
) -> Option<f32> {
    if text_mask.len() != a.len() {
        return None;
    }
    block_mean(a, b, width, height, Some(text_mask))
}

fn block_mean(
    a: &[f32],
    b: &[f32],
    width: usize,
    height: usize,
    text_mask: Option<&[bool]>,
) -> Option<f32> {
    if a.len() != b.len() || a.len() != width.saturating_mul(height) || width == 0 || height == 0 {
        return None;
    }
    const BLOCK: usize = 8;
    let mut scores = Vec::new();
    let mut y0 = 0;
    while y0 < height {
        let mut x0 = 0;
        let y1 = (y0 + BLOCK).min(height);
        while x0 < width {
            let x1 = (x0 + BLOCK).min(width);
            let mut left = Vec::new();
            let mut right = Vec::new();
            let mut text_pixels = 0usize;
            for y in y0..y1 {
                for x in x0..x1 {
                    let index = y * width + x;
                    if text_mask.is_some_and(|mask| mask[index]) {
                        text_pixels += 1;
                    }
                    left.push(a[index]);
                    right.push(b[index]);
                }
            }
            let count = left.len();
            let mostly_text = text_mask.is_some() && text_pixels * 10 > count * 3;
            if count >= 16 && !mostly_text {
                scores.push(window_ssim(&left, &right));
            }
            x0 += BLOCK;
        }
        y0 += BLOCK;
    }
    if scores.is_empty() {
        None
    } else {
        Some(scores.iter().sum::<f32>() / scores.len() as f32)
    }
}

fn window_ssim(left: &[f32], right: &[f32]) -> f32 {
    let n = left.len() as f32;
    let mean_l = left.iter().sum::<f32>() / n;
    let mean_r = right.iter().sum::<f32>() / n;
    let mut var_l = 0.0;
    let mut var_r = 0.0;
    let mut cov = 0.0;
    for (l, r) in left.iter().zip(right) {
        let dl = l - mean_l;
        let dr = r - mean_r;
        var_l += dl * dl;
        var_r += dr * dr;
        cov += dl * dr;
    }
    let denom = (n - 1.0).max(1.0);
    var_l /= denom;
    var_r /= denom;
    cov /= denom;
    ((2.0 * mean_l * mean_r + C1) * (2.0 * cov + C2))
        / ((mean_l * mean_l + mean_r * mean_r + C1) * (var_l + var_r + C2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_image_scores_one() {
        let pixels = vec![40.0; 64];
        let score = mean_ssim(&pixels, &pixels, 8, 8).unwrap();
        assert!((score - 1.0).abs() < 1e-5, "{score}");
        assert!((exact_ratio(&pixels, &pixels) - 1.0).abs() < 1e-6);
    }
}
