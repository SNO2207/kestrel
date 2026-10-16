//! Text in images, read by Windows' own recogniser (`Windows.Media.Ocr`), in the languages the
//! owner has both chosen in Windows and has a recogniser for. Nothing is downloaded: a language
//! without a recogniser installed is simply not read.
//!
//! COM must be initialised on the calling thread ([`crate::Apartment`]); a [`Recognizer`] is
//! made on the thread that uses it.

use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::System::UserProfile::GlobalizationPreferences;
use windows::core::HSTRING;

use crate::decode::Bitmap;

/// The recogniser's engines, one per writing system among the owner's languages.
pub struct Recognizer {
    engines: Vec<OcrEngine>,
    languages: Vec<String>,
    max_dimension: u32,
}

/// The language tags Windows has a text recogniser for on this computer.
pub fn available_languages() -> Vec<String> {
    let Ok(languages) = OcrEngine::AvailableRecognizerLanguages() else { return Vec::new() };
    languages.into_iter().filter_map(|language| language.LanguageTag().ok()).map(|tag| tag.to_string()).collect()
}

impl Recognizer {
    /// Engines for the owner's languages (Windows' language list, in its order) that have a
    /// recogniser, one per writing system: a second language written in the same script is read
    /// well enough by the first's engine, at half the cost. `None` when there is none.
    pub fn for_user() -> Option<Self> {
        let mut engines = Vec::new();
        let mut languages = Vec::new();
        let mut scripts: Vec<String> = Vec::new();
        let preferred: Vec<HSTRING> = GlobalizationPreferences::Languages().map(|list| list.into_iter().collect()).unwrap_or_default();
        for tag in preferred {
            let Ok(language) = Language::CreateLanguage(&tag) else { continue };
            if !OcrEngine::IsLanguageSupported(&language).unwrap_or(false) {
                continue;
            }
            let script = language.Script().map(|s| s.to_string()).unwrap_or_default();
            if scripts.contains(&script) {
                continue;
            }
            if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
                scripts.push(script);
                languages.push(engine.RecognizerLanguage().and_then(|l| l.LanguageTag()).map(|t| t.to_string()).unwrap_or_else(|_| tag.to_string()));
                engines.push(engine);
            }
        }
        if engines.is_empty() {
            let engine = OcrEngine::TryCreateFromUserProfileLanguages().ok()?;
            languages.push(engine.RecognizerLanguage().and_then(|l| l.LanguageTag()).map(|t| t.to_string()).unwrap_or_default());
            engines.push(engine);
        }
        let max_dimension = OcrEngine::MaxImageDimension().unwrap_or(2600);
        Some(Recognizer { engines, languages, max_dimension })
    }

    /// The languages read, as tags such as `en-US`.
    pub fn languages(&self) -> &[String] {
        &self.languages
    }

    /// The largest side an image may have; larger ones are scaled down first.
    pub fn max_dimension(&self) -> u32 {
        self.max_dimension
    }

    /// The text in a picture, a line to a line, or an empty string when there is none.
    pub fn recognize(&self, bitmap: &Bitmap) -> Result<String, String> {
        let fitted;
        let bitmap = if bitmap.width.max(bitmap.height) > self.max_dimension {
            fitted = bitmap.fit(self.max_dimension)?;
            &fitted
        } else {
            bitmap
        };
        if bitmap.width < 8 || bitmap.height < 8 {
            return Ok(String::new());
        }
        let software = software_bitmap(bitmap).map_err(|error| error.message())?;
        let mut lines: Vec<String> = Vec::new();
        for engine in &self.engines {
            let result = engine.RecognizeAsync(&software).and_then(|operation| operation.join()).map_err(|error| error.message())?;
            for line in result.Lines().map_err(|error| error.message())? {
                let text = line.Text().map(|text| text.to_string()).unwrap_or_default();
                let text = text.trim();
                if !text.is_empty() && !lines.iter().any(|seen| seen == text) {
                    lines.push(text.to_owned());
                }
            }
        }
        Ok(lines.join("\n"))
    }
}

/// The pipeline's text stage: every photo is read for text (screenshots, receipts, signs), at
/// most at the recogniser's largest size, and the text is stored for `text:` searches and the
/// details pane. Videos are not read.
pub struct TextStage {
    db: std::sync::Arc<kestrel_db::Db>,
    max_dimension: u32,
}

impl TextStage {
    pub const NAME: &'static str = "text";

    pub fn new(db: std::sync::Arc<kestrel_db::Db>) -> Self {
        let max_dimension = OcrEngine::MaxImageDimension().unwrap_or(2600).clamp(256, 10_000);
        TextStage { db, max_dimension }
    }
}

thread_local! {
    /// Each worker thread's recogniser, made the first time it reads an image; `None` inside
    /// when this computer has no recogniser for the owner's languages.
    static RECOGNIZER: std::cell::OnceCell<Option<Recognizer>> = const { std::cell::OnceCell::new() };
}

/// Why nothing could be read, when no language has a recogniser.
pub const NO_LANGUAGE: &str = "No text recognition language is installed for your Windows languages";

impl crate::pipeline::Stage for TextStage {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        1
    }

    fn applies_to(&self, kind: crate::Kind) -> bool {
        kind == crate::Kind::Photo
    }

    fn needs(&self) -> crate::pipeline::Needs {
        crate::pipeline::Needs { pixels: self.max_dimension }
    }

    fn run(&self, item: &crate::Item, input: &crate::Input) -> Result<(), String> {
        let picture = input.picture.ok_or("No picture to read")?;
        let text = RECOGNIZER.with(|cell| match cell.get_or_init(Recognizer::for_user) {
            Some(recognizer) => recognizer.recognize(picture),
            None => Err(NO_LANGUAGE.to_owned()),
        })?;
        kestrel_db::media::set_text(&self.db, item.id, &text).map_err(|error| error.to_string())
    }
}

fn software_bitmap(bitmap: &Bitmap) -> windows::core::Result<SoftwareBitmap> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bitmap.pixels)?;
    let buffer = writer.DetachBuffer()?;
    SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, bitmap.width as i32, bitmap.height as i32)
}
