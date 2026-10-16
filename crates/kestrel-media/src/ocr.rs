//! Text in images, read by Windows' own recogniser (`Windows.Media.Ocr`), in the languages the
//! owner has both chosen in Windows and has a recogniser for. Nothing is downloaded: a language
//! without a recogniser installed is simply not read.
//!
//! COM must be initialised on the calling thread ([`crate::Apartment`]); a [`Recognizer`] is
//! made on the thread that uses it.
//!
//! Windows recognises text on a thread of its own thread pool, at normal priority, whatever the
//! priority of the thread that asked: the pass's background mode doesn't reach it. So the text
//! stage reads at most [`READ_SIDE`] pixels (a photo's text is legible well below the 10,000
//! the recogniser takes, at a fraction of the work and memory), and waits for the processors to
//! have room ([`Room`]) before asking, rather than competing with the owner's own work.

use std::time::{Duration, Instant};

use parking_lot::Mutex;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapBufferAccessMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::System::UserProfile::GlobalizationPreferences;
use windows::Win32::System::WinRT::IMemoryBufferByteAccess;
use windows::core::{HSTRING, Interface};

use crate::decode::Bitmap;

/// The longest side the text stage reads a photo at. The recogniser takes up to its
/// `MaxImageDimension` (10,000 on Windows 11), but text a phone photo shows legibly is still
/// legible at 4,000; a 48-megapixel photo decoded whole for it was 190 MB a copy.
pub const READ_SIDE: u32 = 4000;

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

/// When the text stage may ask Windows to recognise text: once the processors have room, or
/// once it has waited long enough (so a machine busy for hours still gets its photos read, a
/// recognition every `patience` at most).
pub struct Room {
    pub has_room: Box<dyn Fn() -> bool + Send + Sync>,
    pub recheck: Duration,
    pub patience: Duration,
}

impl Default for Room {
    fn default() -> Self {
        Room { has_room: Box::new(processors_have_room), recheck: Duration::from_millis(250), patience: Duration::from_secs(10) }
    }
}

impl Room {
    /// Waits for room, or for its patience to run out.
    pub fn wait(&self) {
        let started = Instant::now();
        while !(self.has_room)() && started.elapsed() < self.patience {
            std::thread::sleep(self.recheck);
        }
    }
}

/// The system's idle time, and its kernel (which includes idle) and user time, summed over the
/// processors.
#[derive(Clone, Copy)]
struct Times {
    idle: u64,
    all: u64,
}

fn system_times() -> Option<Times> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    let ticks = |time: FILETIME| u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime);
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: three valid FILETIMEs for the call to fill.
    (unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0).then(|| Times { idle: ticks(idle), all: ticks(kernel) + ticks(user) })
}

/// How many processors' worth of time went idle lately: since the last look, when that was a
/// moment ago, or else over a fresh moment. `None` when Windows doesn't say.
pub fn idle_processors() -> Option<f64> {
    const MOMENT: Duration = Duration::from_millis(100);
    const STALE: Duration = Duration::from_secs(2);
    static LAST: Mutex<Option<(Instant, Times)>> = Mutex::new(None);
    let previous = *LAST.lock();
    let from = match previous {
        Some((at, times)) if at.elapsed() >= MOMENT && at.elapsed() <= STALE => times,
        _ => {
            let start = system_times()?;
            std::thread::sleep(MOMENT);
            start
        }
    };
    let now = system_times()?;
    *LAST.lock() = Some((Instant::now(), now));
    let total = now.all.checked_sub(from.all).filter(|&total| total > 0)?;
    let idle = now.idle.saturating_sub(from.idle).min(total);
    let processors = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    Some(idle as f64 / total as f64 * processors)
}

/// Whether at least one processor's worth of time is going idle: otherwise the owner's work
/// has every core, and a recognition would take its share at normal priority.
pub fn processors_have_room() -> bool {
    idle_processors().is_none_or(|idle| idle >= 1.0)
}

/// The pipeline's text stage: every photo is read for text (screenshots, receipts, signs), at
/// most at [`READ_SIDE`], and the text is stored for `text:` searches and the details pane.
/// Videos are not read.
pub struct TextStage {
    db: std::sync::Arc<kestrel_db::Db>,
    max_dimension: u32,
    room: Room,
}

impl TextStage {
    pub const NAME: &'static str = "text";

    pub fn new(db: std::sync::Arc<kestrel_db::Db>) -> Self {
        Self::with_room(db, Room::default())
    }

    /// The stage, asking `room` before each recognition.
    pub fn with_room(db: std::sync::Arc<kestrel_db::Db>, room: Room) -> Self {
        let max_dimension = OcrEngine::MaxImageDimension().unwrap_or(2600).clamp(256, READ_SIDE);
        TextStage { db, max_dimension, room }
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
        let text = crate::priority::background(|| {
            RECOGNIZER.with(|cell| match cell.get_or_init(Recognizer::for_user) {
                Some(recognizer) => {
                    self.room.wait();
                    recognizer.recognize(picture)
                }
                None => Err(NO_LANGUAGE.to_owned()),
            })
        })?;
        kestrel_db::media::set_text(&self.db, item.id, &text).map_err(|error| error.to_string())
    }
}

/// The picture as the recogniser takes it, copied once, straight into the bitmap's own buffer.
fn software_bitmap(bitmap: &Bitmap) -> windows::core::Result<SoftwareBitmap> {
    let software = SoftwareBitmap::Create(BitmapPixelFormat::Bgra8, bitmap.width as i32, bitmap.height as i32)?;
    {
        let buffer = software.LockBuffer(BitmapBufferAccessMode::Write)?;
        let plane = buffer.GetPlaneDescription(0)?;
        let reference = buffer.CreateReference()?;
        let access: IMemoryBufferByteAccess = reference.cast()?;
        let (mut data, mut capacity) = (std::ptr::null_mut(), 0u32);
        // SAFETY: the buffer stays locked for writing while `reference` lives; rows are written
        // at the plane's own start and stride, within the capacity it reports.
        unsafe {
            access.GetBuffer(&mut data, &mut capacity)?;
            let row = bitmap.width as usize * 4;
            let (start, stride) = (plane.StartIndex.max(0) as usize, plane.Stride.max(0) as usize);
            let needed = start + stride * (bitmap.height as usize).saturating_sub(1) + row;
            if data.is_null() || stride < row || needed > capacity as usize {
                return Err(windows::core::Error::new(windows::core::HRESULT(0x8007_000Eu32 as i32), "the bitmap's buffer is too small"));
            }
            let out = std::slice::from_raw_parts_mut(data, capacity as usize);
            for (y, source) in bitmap.pixels.chunks_exact(row).enumerate() {
                out[start + y * stride..][..row].copy_from_slice(source);
            }
        }
        reference.Close()?;
        buffer.Close()?;
    }
    Ok(software)
}
