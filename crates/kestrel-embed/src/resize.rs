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

/// One row of the horizontal pass: `source` (RGB, the input's width) into `out` (RGB, the
/// output's width). `ImagingResampleHorizontal_8bpc` in Pillow.
fn horizontal_row(source: &[u8], horizontal: &Coeffs, out: &mut [u8]) {
    for (xx, &(xmin, xmax)) in horizontal.bounds.iter().enumerate() {
        let k = &horizontal.weights[xx * horizontal.ksize..][..xmax];
        let mut sums = [1i64 << (PRECISION_BITS - 1); 3];
        for (x, &weight) in k.iter().enumerate() {
            for c in 0..3 {
                sums[c] += i64::from(source[(x + xmin) * 3 + c]) * i64::from(weight);
            }
        }
        for c in 0..3 {
            out[xx * 3 + c] = clip8(sums[c]);
        }
    }
}

/// The vertical pass over `pixels` (`w` wide, RGB), whose rows the bounds count from.
/// `ImagingResampleVertical_8bpc` in Pillow.
fn vertical_pass(pixels: &[u8], w: usize, vertical: &Coeffs) -> Vec<u8> {
    let mut out = vec![0u8; w * vertical.bounds.len() * 3];
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
    out
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
        for (yy, row) in out.chunks_exact_mut(out_w * 3).enumerate() {
            horizontal_row(&image.pixels[(yy + first_row) * in_w * 3..][..in_w * 3], &horizontal, row);
        }
        (pixels, w, h) = (out, out_w, rows);
    }
    if in_h != out_h {
        (pixels, h) = (vertical_pass(&pixels, w, &vertical), out_h);
    }
    debug_assert_eq!((w, h), (out_w, out_h));
    Rgb { width, height, pixels }
}

/// Where a stored pixel shows, by the EXIF orientation: whether the picture is transposed (5
/// to 8: stored rows are shown columns), and whether the shown x and y run against the stored
/// ones. The inverse of what `Rgb::oriented` reads.
struct Turn {
    transposed: bool,
    /// Shown x counts down as the stored row (transposed) or column (not) counts up.
    x_back: bool,
    /// Shown y counts down as the stored column (transposed) or row (not) counts up.
    y_back: bool,
}

impl Turn {
    fn of(orientation: u16) -> Turn {
        let (transposed, x_back, y_back) = match orientation {
            2 => (false, true, false),
            3 => (false, true, true),
            4 => (false, false, true),
            5 => (true, false, false),
            6 => (true, true, false),
            7 => (true, true, true),
            8 => (true, false, true),
            _ => (false, false, false),
        };
        Turn { transposed, x_back, y_back }
    }
}

/// Pillow's bilinear resize of a picture handed over a stored row at a time, turned upright by
/// its EXIF orientation on the way: exactly `resize(&stored.oriented(orientation), width,
/// height)`, without ever holding the picture whole. A 48-megapixel photo goes to the model's
/// square through a few tens of megabytes instead of the full picture and its turned copy.
///
/// The horizontal pass (on the upright picture) is done as rows arrive: a stored row is an
/// upright row, or for a turned picture an upright column, whose pixels are added into every
/// output column they weigh on. Integer sums add up the same in any order, so the result is
/// Pillow's to the bit.
pub struct Streaming {
    stored_w: usize,
    stored_h: usize,
    turn: Turn,
    out_w: usize,
    out_h: usize,
    /// The upright rows the vertical pass reads, from `first_row`.
    first_row: usize,
    rows: usize,
    horizontal: Option<Coeffs>,
    vertical: Option<Coeffs>,
    /// Not turned: the rows after the horizontal pass (or as they are, without one), RGB.
    plain: Vec<u8>,
    /// Turned: the horizontal pass's sums, by output column, then row, then channel.
    sums: Vec<i32>,
    /// Turned, without a horizontal pass: the rows as they are, by column, then row.
    columns: Vec<u8>,
    /// For each upright x, the output columns it weighs on: `(column, weight)` from
    /// `weigh_from[x]` to `weigh_from[x + 1]`.
    weigh_from: Vec<usize>,
    weighs: Vec<(u32, i32)>,
    /// One stored row turned into an upright one, RGB (not turned only).
    row: Vec<u8>,
}

impl Streaming {
    /// A resize to `width` x `height` of a `stored_w` x `stored_h` picture shown turned by EXIF
    /// `orientation`. `None` for an empty size.
    pub fn new(stored_w: u32, stored_h: u32, orientation: u16, width: u32, height: u32) -> Option<Streaming> {
        let (stored_w, stored_h, out_w, out_h) = (stored_w as usize, stored_h as usize, width as usize, height as usize);
        if stored_w == 0 || stored_h == 0 || out_w == 0 || out_h == 0 {
            return None;
        }
        let turn = Turn::of(orientation);
        let (up_w, up_h) = if turn.transposed { (stored_h, stored_w) } else { (stored_w, stored_h) };
        let horizontal = (up_w != out_w).then(|| coefficients(up_w, out_w));
        let mut vertical = (up_h != out_h).then(|| coefficients(up_h, out_h));
        let (first_row, last_row) = match &vertical {
            Some(vertical) => (vertical.bounds[0].0, vertical.bounds[out_h - 1].0 + vertical.bounds[out_h - 1].1),
            None => (0, up_h),
        };
        if let Some(vertical) = &mut vertical {
            for bound in &mut vertical.bounds {
                bound.0 -= first_row;
            }
        }
        let rows = last_row - first_row;
        let inner_w = if horizontal.is_some() { out_w } else { up_w };
        let mut weigh_from = vec![0usize; up_w + 1];
        let mut weighs = Vec::new();
        if turn.transposed
            && let Some(horizontal) = &horizontal
        {
            let mut by_x: Vec<Vec<(u32, i32)>> = vec![Vec::new(); up_w];
            for (xx, &(xmin, xmax)) in horizontal.bounds.iter().enumerate() {
                for (k, &weight) in horizontal.weights[xx * horizontal.ksize..][..xmax].iter().enumerate() {
                    by_x[xmin + k].push((xx as u32, weight));
                }
            }
            for (x, list) in by_x.into_iter().enumerate() {
                weighs.extend(list);
                weigh_from[x + 1] = weighs.len();
            }
        }
        let (plain, sums, columns) = match (turn.transposed, horizontal.is_some()) {
            (false, _) => (vec![0u8; rows * inner_w * 3], Vec::new(), Vec::new()),
            (true, true) => (Vec::new(), vec![1i32 << (PRECISION_BITS - 1); out_w * rows * 3], Vec::new()),
            (true, false) => (Vec::new(), Vec::new(), vec![0u8; up_w * rows * 3]),
        };
        let row = if turn.transposed { Vec::new() } else { vec![0u8; up_w * 3] };
        Some(Streaming {
            stored_w,
            stored_h,
            turn,
            out_w,
            out_h,
            first_row,
            rows,
            horizontal,
            vertical,
            plain,
            sums,
            columns,
            weigh_from,
            weighs,
            row,
        })
    }

    /// The stored rows the result needs, as a range: the others need not be decoded at all.
    pub fn stored_rows(&self) -> std::ops::Range<usize> {
        if self.turn.transposed {
            // A stored row is an upright column: every one counts.
            return 0..self.stored_h;
        }
        let (first, last) = (self.first_row, self.first_row + self.rows);
        if self.turn.y_back { self.stored_h - last..self.stored_h - first } else { first..last }
    }

    /// Takes stored row `y`: `stored_w` pixels, four bytes each, red, green and blue first
    /// (the fourth is ignored, as the reference's RGB conversion drops alpha).
    pub fn feed(&mut self, y: usize, rgba: &[u8]) {
        debug_assert!(rgba.len() >= self.stored_w * 4 && y < self.stored_h);
        let (w, h) = (self.stored_w, self.stored_h);
        if !self.turn.transposed {
            let upright = if self.turn.y_back { h - 1 - y } else { y };
            let Some(r) = upright.checked_sub(self.first_row).filter(|&r| r < self.rows) else { return };
            for x in 0..w {
                let from = if self.turn.x_back { w - 1 - x } else { x };
                self.row[x * 3..x * 3 + 3].copy_from_slice(&rgba[from * 4..from * 4 + 3]);
            }
            match &self.horizontal {
                Some(horizontal) => horizontal_row(&self.row, horizontal, &mut self.plain[r * self.out_w * 3..][..self.out_w * 3]),
                None => self.plain[r * w * 3..][..w * 3].copy_from_slice(&self.row),
            }
            return;
        }
        // Turned: this stored row is upright column `x`, and stored column `sx` upright row
        // `sx` (or counting back).
        let x = if self.turn.x_back { h - 1 - y } else { y };
        let rows = self.rows;
        let upright_row = |sx: usize| if self.turn.y_back { w - 1 - sx } else { sx };
        if self.horizontal.is_some() {
            for &(xx, weight) in &self.weighs[self.weigh_from[x]..self.weigh_from[x + 1]] {
                let column = &mut self.sums[xx as usize * rows * 3..][..rows * 3];
                for sx in 0..w {
                    let Some(r) = upright_row(sx).checked_sub(self.first_row).filter(|&r| r < rows) else { continue };
                    for c in 0..3 {
                        column[r * 3 + c] += i32::from(rgba[sx * 4 + c]) * weight;
                    }
                }
            }
        } else {
            let column = &mut self.columns[x * rows * 3..][..rows * 3];
            for sx in 0..w {
                let Some(r) = upright_row(sx).checked_sub(self.first_row).filter(|&r| r < rows) else { continue };
                column[r * 3..r * 3 + 3].copy_from_slice(&rgba[sx * 4..sx * 4 + 3]);
            }
        }
    }

    /// The resized picture, once every row in [`Streaming::stored_rows`] has been fed.
    pub fn finish(self) -> Rgb {
        let inner_w = if self.horizontal.is_some() { self.out_w } else if self.turn.transposed { self.stored_h } else { self.stored_w };
        let rows = self.rows;
        let inner = if !self.turn.transposed {
            self.plain
        } else {
            // By column back to by row.
            let mut inner = vec![0u8; rows * inner_w * 3];
            for column in 0..inner_w {
                for r in 0..rows {
                    for c in 0..3 {
                        let at = (column * rows + r) * 3 + c;
                        inner[(r * inner_w + column) * 3 + c] =
                            if self.horizontal.is_some() { clip8(i64::from(self.sums[at])) } else { self.columns[at] };
                    }
                }
            }
            inner
        };
        let pixels = match &self.vertical {
            Some(vertical) => vertical_pass(&inner, inner_w, vertical),
            None => inner,
        };
        debug_assert_eq!(pixels.len(), self.out_w * self.out_h * 3);
        Rgb { width: self.out_w as u32, height: self.out_h as u32, pixels }
    }
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

    /// A picture of repeatable noise, so any pixel out of place shows.
    fn noise(width: u32, height: u32, seed: u64) -> Rgb {
        let mut state = seed | 1;
        let pixels = (0..width * height * 3)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect();
        Rgb { width, height, pixels }
    }

    /// Fed a stored row at a time and turned on the way, the result is the reference's to the
    /// bit: the picture turned upright whole, then resized.
    #[test]
    fn streaming_matches_turning_then_resizing() {
        let shapes = [(97, 61, 16, 16), (40, 90, 16, 16), (61, 97, 384, 384), (33, 20, 33, 7), (20, 33, 5, 33), (12, 9, 12, 9), (9, 12, 12, 9)];
        for (n, &(w, h, out_w, out_h)) in shapes.iter().enumerate() {
            let stored = noise(w, h, n as u64 + 7);
            for orientation in 1..=8u16 {
                let expected = resize(&stored.clone().oriented(orientation), out_w, out_h);
                let mut streaming = Streaming::new(w, h, orientation, out_w, out_h).unwrap();
                let rows = streaming.stored_rows();
                for y in rows {
                    let rgba: Vec<u8> = stored.pixels[y * w as usize * 3..][..w as usize * 3].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 0x55]).collect();
                    streaming.feed(y, &rgba);
                }
                assert_eq!(streaming.finish(), expected, "{w}x{h} to {out_w}x{out_h}, orientation {orientation}");
            }
        }
        assert!(Streaming::new(0, 5, 1, 4, 4).is_none());
    }
}
