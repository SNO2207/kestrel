//! Text in pictures, read by Windows' own recogniser (`Windows.Media.Ocr`, with the languages
//! the owner's profile has; English is on this machine). It runs on the processor and needs
//! nothing downloaded. A description's words found in a photo's text count toward that photo
//! in a search, so "costco 42.17" finds the receipt even where the picture alone is not enough.

use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;

use crate::{Error, Result, Rgb};

/// A recogniser for the owner's languages.
pub struct Ocr {
    engine: OcrEngine,
    max_side: u32,
}

impl Ocr {
    /// The recogniser for the owner's profile languages, or `None` when Windows has none.
    pub fn new() -> Result<Ocr> {
        crate::com();
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|_| Error("Windows has no text recogniser for this profile's languages".into()))?;
        let max_side = OcrEngine::MaxImageDimension().unwrap_or(2600);
        Ok(Ocr { engine, max_side })
    }

    /// The text in `image`, its lines joined by spaces; empty when there is none.
    pub fn read(&self, image: &Rgb) -> Result<String> {
        let longest = image.width.max(image.height);
        let fitted;
        let image = if longest > self.max_side {
            let scale = self.max_side as f32 / longest as f32;
            let (w, h) = (((image.width as f32 * scale) as u32).max(1), ((image.height as f32 * scale) as u32).max(1));
            fitted = crate::resize::resize(image, w, h);
            &fitted
        } else {
            image
        };
        if image.width < 40 || image.height < 40 {
            return Ok(String::new());
        }
        let bgra: Vec<u8> = image.pixels.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        let writer = DataWriter::new()?;
        writer.WriteBytes(&bgra)?;
        let buffer = writer.DetachBuffer()?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, image.width as i32, image.height as i32)?;
        let result = self.engine.RecognizeAsync(&bitmap)?.join()?;
        let mut words = Vec::new();
        for line in result.Lines()? {
            words.push(line.Text()?.to_string_lossy());
        }
        Ok(words.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printed_words_are_read() {
        let ocr = Ocr::new().expect("this machine has the English recogniser");
        let picture = crate::draw::words(900, 300, [255, 255, 255], [0, 0, 0], &["COSTCO WHOLESALE", "TOTAL 42.17"], 48).unwrap();
        let text = ocr.read(&picture).unwrap().to_lowercase();
        assert!(text.contains("costco") && text.contains("42.17"), "{text}");
        assert_eq!(ocr.read(&Rgb::filled(300, 300, [90, 140, 200])).unwrap(), "");
    }
}
