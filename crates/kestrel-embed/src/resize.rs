//! Resizing exactly as the reference image processor does: Pillow's bilinear resampling of an
//! 8-bit RGB picture (`Image.resize(size, BILINEAR)`), which is what SigLIP's processor calls.
//!
//! Pillow's "bilinear" is a triangle filter whose support widens with the reduction, so a
//! picture made smaller is averaged over every source pixel, not sampled at four. It works in
//! two passes, horizontal then vertical, each with coefficients in fixed point (22 fractional
//! bits) and its result rounded to 8 bits, and only over the rows the vertical pass will read.
//! This module follows `Resample.c` step by step, rounding included, so the model sees the
//! numbers it was trained on.

use crate::Rgb;

const PRECISION_BITS: u32 = 32 - 8 - 2;

fn bilinear(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 { 1.0 - x } else { 0.0 }
}

/// For each output pixel along one axis: the first source pixel, how many it reads, and their
/// weights in fixed point. `precompute_coeffs` and `normalize_coeffs_8bpc` in Pillow.
struct Coeffs {
    ksize: usize,
    bounds: Vec<(usize, usize)>,
    weights: Vec<i32>,
}

fn coefficients(in_size: usize, out_size: usize) -> Coeffs {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = filterscale; // the bilinear filter's support is 1
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(out_size);
    let mut weights = vec![0i32; out_size * ksize];
    let mut k = vec![0f64; ksize];
    for xx in 0..out_size {
        let center = (xx as f64 + 0.5) * scale;
        let ss = 1.0 / filterscale;
        // C's casts truncate toward zero, as `as` does.
        let xmin = ((center - support + 0.5) as i64).max(0) as usize;
        let xmax = ((center + support + 0.5) as i64).min(in_size as i64) as usize - xmin;
        let mut ww = 0.0;
        for (x, slot) in k.iter_mut().enumerate().take(xmax) {
            let w = bilinear((x as f64 + xmin as f64 - center + 0.5) * ss);
            *slot = w;
            ww += w;
        }
        for x in 0..ksize {
            let w = if x < xmax && ww != 0.0 { k[x] / ww } else { 0.0 };
            let fixed = w * f64::from(1u32 << PRECISION_BITS);
            weights[xx * ksize + x] = if w < 0.0 { (fixed - 0.5) as i32 } else { (fixed + 0.5) as i32 };
        }
        bounds.push((xmin, xmax));
    }
    Coeffs { ksize, bounds, weights }
}

fn clip8(value: i64) -> u8 {
    if value >= (1i64 << PRECISION_BITS << 8) {
        255
    } else if value <= 0 {
        0
    } else {
        (value >> PRECISION_BITS) as u8
    }
}

/// `image` resized to `width` x `height` as Pillow's bilinear resize makes it.
pub fn resize(image: &Rgb, width: u32, height: u32) -> Rgb {
    let (in_w, in_h) = (image.width as usize, image.height as usize);
    let (out_w, out_h) = (width as usize, height as usize);
    if (in_w, in_h) == (out_w, out_h) || in_w == 0 || in_h == 0 || out_w == 0 || out_h == 0 {
        return if (in_w, in_h) == (out_w, out_h) { image.clone() } else { Rgb::filled(width, height, [0; 3]) };
    }
    let horizontal = coefficients(in_w, out_w);
    let mut vertical = coefficients(in_h, out_h);
    // Only the rows the vertical pass reads go through the horizontal one.
    let first_row = vertical.bounds[0].0;
    let last_row = vertical.bounds[out_h - 1].0 + vertical.bounds[out_h - 1].1;

    let (mut pixels, mut w, mut h) = (image.pixels.clone(), in_w, in_h);
    if in_w != out_w {
        for bound in &mut vertical.bounds {
            bound.0 -= first_row;
        }
        let rows = last_row - first_row;
        let mut out = vec![0u8; out_w * rows * 3];
        for yy in 0..rows {
            let source = &image.pixels[(yy + first_row) * in_w * 3..][..in_w * 3];
            for (xx, &(xmin, xmax)) in horizontal.bounds.iter().enumerate() {
                let k = &horizontal.weights[xx * horizontal.ksize..][..xmax];
                let mut sums = [1i64 << (PRECISION_BITS - 1); 3];
                for (x, &weight) in k.iter().enumerate() {
                    for c in 0..3 {
                        sums[c] += i64::from(source[(x + xmin) * 3 + c]) * i64::from(weight);
                    }
                }
                for c in 0..3 {
                    out[(yy * out_w + xx) * 3 + c] = clip8(sums[c]);
                }
            }
        }
        (pixels, w, h) = (out, out_w, rows);
    }
    if in_h != out_h {
        let mut out = vec![0u8; w * out_h * 3];
        for (yy, &(ymin, ymax)) in vertical.bounds.iter().enumerate() {
            let k = &vertical.weights[yy * vertical.ksize..][..ymax];
            for xx in 0..w {
                let mut sums = [1i64 << (PRECISION_BITS - 1); 3];
                for (y, &weight) in k.iter().enumerate() {
                    for c in 0..3 {
                        sums[c] += i64::from(pixels[((y + ymin) * w + xx) * 3 + c]) * i64::from(weight);
                    }
                }
                for c in 0..3 {
                    out[(yy * w + xx) * 3 + c] = clip8(sums[c]);
                }
            }
        }
        (pixels, h) = (out, out_h);
    }
    debug_assert_eq!((w, h), (out_w, out_h));
    Rgb { width, height, pixels }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey_row(values: &[u8]) -> Rgb {
        Rgb { width: values.len() as u32, height: 1, pixels: values.iter().flat_map(|&v| [v, v, v]).collect() }
    }

    fn grey(image: &Rgb) -> Vec<u8> {
        image.pixels.chunks_exact(3).map(|p| p[0]).collect()
    }

    /// Halving [0, 100, 200, 255]: each output reads three pixels with weights 0.75, 0.75 and
    /// 0.25 over 1.75 (the support is 2 when halving), so 0.4286 x 0 + 0.4286 x 100 +
    /// 0.1429 x 200 = 71.43, rounded down after the fixed point's half is added: 71; and
    /// 0.1429 x 100 + 0.4286 x 200 + 0.4286 x 255 = 209.29: 209.
    #[test]
    fn halving_a_row_matches_pillow_by_hand() {
        assert_eq!(grey(&resize(&grey_row(&[0, 100, 200, 255]), 2, 1)), [71, 209]);
    }

    /// Doubling [0, 200]: the ends copy their pixel, the middle two mix 3:1 and 1:3, so 50 and
    /// 150 (exactly 50.5 and 150.5 in fixed point, rounded down).
    #[test]
    fn doubling_a_row_matches_pillow_by_hand() {
        assert_eq!(grey(&resize(&grey_row(&[0, 200]), 4, 1)), [0, 50, 150, 200]);
    }

    /// The horizontal pass is rounded to 8 bits before the vertical one, as in Pillow:
    /// [[10, 20], [30, 41]] to one pixel is (10 + 20 + 1) / 2 = 15 and (30 + 41 + 1) / 2 = 36,
    /// then (15 + 36 + 1) / 2 = 26, where the exact mean would round to 25.
    #[test]
    fn the_two_passes_round_in_between() {
        let image = Rgb { width: 2, height: 2, pixels: [10u8, 20, 30, 41].iter().flat_map(|&v| [v, v, v]).collect() };
        assert_eq!(grey(&resize(&image, 1, 1)), [26]);
    }

    #[test]
    fn colours_stay_apart_and_a_flat_picture_stays_flat() {
        let mut image = Rgb::filled(300, 200, [12, 200, 77]);
        assert!(resize(&image, 256, 256).pixels.chunks_exact(3).all(|p| p == [12, 200, 77]));
        // Left half red, right half blue: the left edge stays pure red, the right pure blue.
        for y in 0..200 {
            for x in 0..300 {
                image.set(x, y, if x < 150 { [255, 0, 0] } else { [0, 0, 255] });
            }
        }
        let small = resize(&image, 16, 16);
        assert_eq!(small.at(0, 8), [255, 0, 0]);
        assert_eq!(small.at(15, 8), [0, 0, 255]);
        let middle = small.at(8, 8);
        assert!(middle[1] == 0 && middle[0] < 255 && middle[2] > 0, "{middle:?}");
    }
}
