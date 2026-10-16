//! A picture as the vision tower takes it: resized to its square (Pillow's bilinear, see
//! [`crate::resize`]), each value multiplied by the rescale factor and normalised,
//! `(v / 255 - mean) / std`, channels first (NCHW). The arithmetic is the reference
//! processor's: the rescale in double precision, then single precision for the rest.

use crate::Rgb;
use crate::catalog::ImageSpec;
use crate::resize::resize;

/// The normalised value of one 8-bit sample on `channel`.
pub fn normalise(value: u8, channel: usize, spec: &ImageSpec) -> f32 {
    let rescaled = (f64::from(value) * f64::from(spec.rescale)) as f32;
    (rescaled - spec.mean[channel]) / spec.std[channel]
}

/// Writes one picture, already the model's size, into `out` as three planes (R, G, B).
pub fn planes(image: &Rgb, spec: &ImageSpec, out: &mut [f32]) {
    let area = (image.width * image.height) as usize;
    debug_assert_eq!(out.len(), area * 3);
    // A table per channel: 256 values, computed once.
    let tables: Vec<[f32; 256]> =
        (0..3).map(|c| std::array::from_fn(|v| normalise(v as u8, c, spec))).collect();
    for (i, pixel) in image.pixels.chunks_exact(3).enumerate() {
        for c in 0..3 {
            out[c * area + i] = tables[c][pixel[c] as usize];
        }
    }
}

/// A batch of pictures as one NCHW tensor of `side` x `side` (the model's input size, which
/// is its declared one, else the catalogue's).
pub fn batch(images: &[Rgb], side: u32, spec: &ImageSpec) -> Vec<f32> {
    let area = (side * side) as usize;
    let mut out = vec![0f32; images.len() * 3 * area];
    for (image, slot) in images.iter().zip(out.chunks_exact_mut(3 * area)) {
        let sized = if image.width == side && image.height == side { image.clone() } else { resize(image, side, side) };
        planes(&sized, spec, slot);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    #[test]
    fn values_are_rescaled_and_normalised_as_siglip_expects() {
        let spec = catalog::fast().image;
        // (v / 255 - 0.5) / 0.5, by hand.
        assert_eq!(normalise(0, 0, &spec), -1.0);
        assert_eq!(normalise(255, 1, &spec), 1.0);
        assert!((normalise(128, 2, &spec) - 0.003_921_628).abs() < 1e-7);
        assert!((normalise(51, 0, &spec) - (-0.6)).abs() < 1e-6);
    }

    #[test]
    fn a_batch_is_nchw_with_planes_in_rgb_order() {
        let spec = catalog::fast().image;
        // A 2 x 1 picture: a red pixel, then a blue one; and a plain grey one.
        let pair = Rgb { width: 2, height: 1, pixels: vec![255, 0, 0, 0, 0, 255] };
        let flat = Rgb::filled(2, 1, [51, 51, 51]);
        // Already 2 x 2? No: the side is 2, so the 2 x 1 picture is stretched to 2 x 2, which
        // repeats its row (Pillow's resize of a single row copies it).
        let tensor = batch(&[pair, flat], 2, &spec);
        assert_eq!(tensor.len(), 2 * 3 * 4);
        let first = &tensor[..12];
        // Red plane: 1 on the left column, -1 on the right; blue plane the other way round.
        assert_eq!(&first[0..4], [1.0, -1.0, 1.0, -1.0]);
        assert_eq!(&first[4..8], [-1.0; 4]);
        assert_eq!(&first[8..12], [-1.0, 1.0, -1.0, 1.0]);
        assert!(tensor[12..].iter().all(|&v| (v + 0.6).abs() < 1e-6));
    }
}
