//! Embeddings of photos, video frames and descriptions, so that "a dog on a sofa" finds the
//! pictures that show one.
//!
//! The models are Google's SigLIP 2 image and text towers ([`catalog`]), downloaded only when the
//! owner asks ([`download`]), and run through Windows ML ([`winml`]): the ONNX Runtime with
//! DirectML that ships with Windows, on the high-performance GPU, or on the CPU when that fails.
//! Nothing else is downloaded or installed to run them.
//!
//! What goes in must be exactly what the models were trained on, or results silently get worse:
//!
//! - images ([`image`], [`resize`], [`preprocess`]): decoded by WIC, turned upright by their
//!   EXIF orientation, RGB, resized to the model's square with the same bilinear filter (and
//!   8-bit rounding) as the reference processor (Pillow's), scaled to 0..1 and normalised with
//!   mean 0.5 and standard deviation 0.5, laid out NCHW in the element type the model declares;
//! - text ([`tokenize`]): lower-cased, tokenised by the model's own `tokenizer.json`, and padded
//!   with its pad token to exactly 64 tokens (SigLIP pools the last position, so the padding is
//!   part of the meaning);
//! - and both towers' outputs are L2-normalised, so a dot product is a cosine.
//!
//! [`selftest`] checks all of that end to end with pictures it draws itself; [`video`] samples
//! frames from videos with Media Foundation; [`onnx`] writes tiny ONNX models with the real
//! towers' inputs and outputs, for tests and development builds.

pub mod catalog;
pub mod download;
pub mod draw;
pub mod image;
pub mod onnx;
pub mod preprocess;
pub mod resize;
pub mod selftest;
pub mod siglip;
pub mod tokenize;
pub mod video;
pub mod winml;

use std::sync::Once;

pub use image::Rgb;
pub use siglip::Siglip;

/// What went wrong, in words fit to show the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<windows_core::Error> for Error {
    fn from(error: windows_core::Error) -> Self {
        Error(format!("{} (0x{:08X})", error.message().trim(), error.code().0))
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Where a model runs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    /// Whether it runs on a graphics card (through DirectML) rather than the processor.
    pub gpu: bool,
    /// The graphics card's name, or "CPU".
    pub name: String,
    /// Why it runs on the CPU, when a GPU was wanted.
    pub note: Option<String>,
}

/// Turns pictures and descriptions into unit vectors of the same space.
pub trait Embedder: Send + Sync {
    /// The length of every vector.
    fn dim(&self) -> usize;
    /// One vector per image, L2-normalised.
    fn embed_images(&self, images: &[Rgb]) -> Result<Vec<Vec<f32>>>;
    /// One vector per text, L2-normalised.
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    /// Where it runs.
    fn device(&self) -> DeviceInfo;
}

/// Scales `vector` to unit length; a zero vector stays zero.
pub fn l2_normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        vector.iter_mut().for_each(|v| *v /= norm);
    }
}

/// The dot product of two vectors of the same length.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Makes COM and WinRT usable from any thread of the process: an implicit multithreaded
/// apartment is kept alive, which threads that never initialised COM themselves join. The app's
/// main thread is a single-threaded apartment (drag and drop needs one) and is never used here.
pub fn com() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: CoIncrementMTAUsage takes an out parameter only. The cookie is never handed to
        // CoDecrementMTAUsage, so the apartment lasts as long as the process.
        let _cookie = unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() };
    });
}
