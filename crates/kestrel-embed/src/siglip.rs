//! SigLIP 2's two towers as one [`Embedder`]: the vision tower for pictures and video frames,
//! the text tower and its tokenizer for descriptions, both on the device Windows ML gave them.
//!
//! What each tower is fed is read from the model itself rather than assumed: the vision tower's
//! picture size and element type (float16 or float32) from its declared input, the vector
//! length from its declared output, and whether the batch is free. A tower that declares an
//! `attention_mask` gets all ones, which is what SigLIP was trained with (no mask).

use std::path::Path;

use rayon::prelude::*;

use crate::catalog::{ImageSpec, ModelSpec};
use crate::tokenize::Tokens;
use crate::winml::{Elem, Feature, Input, Session, Want};
use crate::{DeviceInfo, Embedder, Error, Result, Rgb, l2_normalize, preprocess};

/// Outputs that hold the pooled embedding, in the order they are looked for.
const VISION_OUTPUTS: [&str; 2] = ["image_embeds", "pooler_output"];
const TEXT_OUTPUTS: [&str; 2] = ["text_embeds", "pooler_output"];

pub struct Siglip {
    vision: Session,
    text: Session,
    tokens: Tokens,
    image: ImageSpec,
    /// The vision tower's picture side.
    side: u32,
    vision_input: String,
    vision_output: String,
    text_output: String,
    /// Pictures and descriptions run at most this many at a time.
    image_batch: usize,
    text_batch: usize,
    dim: usize,
    masked: bool,
}

/// The pooled output among `outputs`: a known name, else the first two-dimensional float.
fn pooled(outputs: &[Feature], names: &[&str]) -> Option<Feature> {
    names
        .iter()
        .find_map(|name| outputs.iter().find(|f| f.name == *name))
        .or_else(|| outputs.iter().find(|f| f.shape.len() == 2 && matches!(f.elem, Elem::F32 | Elem::F16)))
        .cloned()
}

/// How many items a tower takes at once: its fixed batch, or `free` when the batch is free.
fn batch_of(feature: &Feature, free: usize) -> usize {
    match feature.shape.first() {
        Some(&n) if n > 0 => n as usize,
        _ => free,
    }
}

impl Siglip {
    /// Opens the model whose files are under `dir`, laid out as `spec` names them.
    pub fn open(dir: &Path, spec: &ModelSpec, want: Want) -> Result<Siglip> {
        let path = |role: &str| -> Result<std::path::PathBuf> {
            Ok(dir.join(spec.file(role).ok_or_else(|| Error(format!("the catalogue names no {role} file")))?.path))
        };
        let vision = Session::load(&path("vision")?, want)?;
        let text = Session::load(&path("text")?, want)?;
        let tokens = Tokens::open(&path("tokenizer")?, spec.text_length)?;

        let input = vision
            .inputs
            .iter()
            .find(|f| f.name == "pixel_values")
            .or_else(|| vision.inputs.iter().find(|f| f.shape.len() == 4))
            .cloned()
            .ok_or_else(|| Error("the vision model takes no picture".into()))?;
        if input.shape.len() != 4 || !(input.shape[1] == 3 || input.shape[1] < 0) {
            return Err(Error(format!("the vision model's input {} is {:?}, not pictures", input.name, input.shape)));
        }
        let side = match (input.shape[2], input.shape[3]) {
            (h, w) if h > 0 && h == w => h as u32,
            _ => spec.image.size,
        };
        let vision_output = pooled(&vision.outputs, &VISION_OUTPUTS).ok_or_else(|| Error("the vision model has no pooled output".into()))?;
        let text_output = pooled(&text.outputs, &TEXT_OUTPUTS).ok_or_else(|| Error("the text model has no pooled output".into()))?;
        let ids = text.input("input_ids").cloned().ok_or_else(|| Error("the text model takes no input_ids".into()))?;
        let dim = match vision_output.shape.get(1) {
            Some(&d) if d > 0 => d as usize,
            _ => spec.dim,
        };
        if text_output.shape.get(1).is_some_and(|&d| d > 0 && d as usize != dim) {
            return Err(Error(format!("the towers' vectors differ in length ({:?} and {:?})", vision_output.shape, text_output.shape)));
        }
        let masked = text.input("attention_mask").is_some();
        Ok(Siglip {
            image_batch: batch_of(&input, if side <= 256 { 16 } else { 8 }),
            text_batch: batch_of(&ids, 16),
            vision_input: input.name,
            vision_output: vision_output.name,
            text_output: text_output.name,
            vision,
            text,
            tokens,
            image: spec.image,
            side,
            dim,
            masked,
        })
    }

    /// The side of the square the vision tower sees.
    pub fn side(&self) -> u32 {
        self.side
    }

    /// The text tower's device, when it differs from the vision tower's.
    pub fn text_device(&self) -> &DeviceInfo {
        self.text.device()
    }

    fn rows(&self, data: Vec<f32>, shape: &[i64], expected: usize) -> Result<Vec<Vec<f32>>> {
        let dim = shape.get(1).copied().unwrap_or(0) as usize;
        if shape.len() != 2 || shape[0] as usize != expected || dim != self.dim || data.len() != expected * dim {
            return Err(Error(format!("the model answered with {shape:?}, not {expected} vectors of {}", self.dim)));
        }
        Ok(data
            .chunks_exact(dim)
            .map(|row| {
                let mut row = row.to_vec();
                l2_normalize(&mut row);
                row
            })
            .collect())
    }
}

impl Embedder for Siglip {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_images(&self, images: &[Rgb]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(images.len());
        for chunk in images.chunks(self.image_batch.max(1)) {
            let area = (self.side * self.side) as usize;
            // Each picture is resized and normalised on a thread of its own.
            let mut tensor = vec![0f32; chunk.len() * 3 * area];
            tensor.par_chunks_mut(3 * area).zip(chunk.par_iter()).for_each(|(slot, image)| {
                slot.copy_from_slice(&preprocess::batch(std::slice::from_ref(image), self.side, &self.image));
            });
            let shape = vec![chunk.len() as i64, 3, i64::from(self.side), i64::from(self.side)];
            let (data, shape) = self.vision.run(&[(&self.vision_input, Input::Floats { shape, data: &tensor })], &self.vision_output)?;
            out.extend(self.rows(data, &shape, chunk.len())?);
        }
        Ok(out)
    }

    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        let length = self.tokens.length() as i64;
        for chunk in texts.chunks(self.text_batch.max(1)) {
            let ids = self.tokens.batch(chunk)?;
            let shape = vec![chunk.len() as i64, length];
            let ones = vec![1i64; ids.len()];
            let mut inputs = vec![("input_ids", Input::Ints { shape: shape.clone(), data: &ids })];
            if self.masked {
                inputs.push(("attention_mask", Input::Ints { shape, data: &ones }));
            }
            let (data, shape) = self.text.run(&inputs, &self.text_output)?;
            out.extend(self.rows(data, &shape, chunk.len())?);
        }
        Ok(out)
    }

    fn device(&self) -> DeviceInfo {
        let mut device = self.vision.device().clone();
        if self.text.device().gpu != device.gpu && device.note.is_none() {
            device.note = self.text.device().note.clone();
        }
        device
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog, dot, onnx};

    fn tiny(half: bool, want: Want) -> (Siglip, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kestrel-embed-siglip-{half}-{want:?}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        onnx::write_tiny_model(&dir, half).unwrap();
        (Siglip::open(&dir, &catalog::fast(), want).unwrap(), dir)
    }

    /// The whole path, picture to unit vector and description to unit vector, against the
    /// tiny model's arithmetic done by hand.
    #[test]
    fn pictures_and_descriptions_come_out_as_the_tiny_model_says() {
        for half in [false, true] {
            let (model, dir) = tiny(half, Want::Gpu);
            assert_eq!((model.dim(), model.side()), (onnx::TINY_DIM, onnx::TINY_SIZE));
            assert!(model.device().gpu);
            let mut striped = Rgb::filled(80, 80, [240, 240, 240]);
            for y in (0..80).step_by(4) {
                for x in 0..80 {
                    striped.set(x, y, [10, 10, 10]);
                }
            }
            let images = vec![Rgb::filled(100, 60, [200, 30, 40]), striped.clone(), Rgb::filled(64, 64, [0, 0, 255])];
            let got = model.embed_images(&images).unwrap();
            for (image, vector) in images.iter().zip(&got) {
                let sized = crate::resize::resize(image, onnx::TINY_SIZE, onnx::TINY_SIZE);
                let mut expected = onnx::tiny_image_features(&sized).to_vec();
                l2_normalize(&mut expected);
                let tolerance = if half { 1e-2 } else { 1e-4 };
                assert!(vector.iter().zip(&expected).all(|(a, b)| (a - b).abs() < tolerance), "{vector:?} vs {expected:?}");
                assert!((dot(vector, vector) - 1.0).abs() < 1e-3);
            }
            let texts = model.embed_texts(&["A RED square", "blue"]).unwrap();
            let ids = model.tokens.ids("a red square").unwrap();
            let mut expected = onnx::tiny_text_features(&ids).to_vec();
            l2_normalize(&mut expected);
            assert!(texts[0].iter().zip(&expected).all(|(a, b)| (a - b).abs() < 1e-2), "{:?} vs {expected:?}", texts[0]);
            // Red finds the red picture before the blue one, and blue the other way round.
            assert!(dot(&texts[0], &got[0]) > dot(&texts[0], &got[2]));
            assert!(dot(&texts[1], &got[2]) > dot(&texts[1], &got[0]));
            drop(model);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn more_pictures_than_a_batch_all_come_back_in_order() {
        let (model, dir) = tiny(false, Want::Cpu);
        let images: Vec<Rgb> = (0..37).map(|i| Rgb::filled(20, 20, [i as u8 * 6, 100, 255 - i as u8 * 6])).collect();
        let got = model.embed_images(&images).unwrap();
        assert_eq!(got.len(), 37);
        // Redder pictures lean further toward red.
        assert!(got[36][0] > got[0][0]);
        drop(model);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
