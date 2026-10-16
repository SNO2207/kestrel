//! Pixels: images decoded by WIC and video frames by Media Foundation, as BGRA bitmaps turned
//! the way they are shown, and bitmaps scaled and encoded again (JPEG and PNG) by WIC.
//!
//! The pipeline decodes each file once, at the largest size any of its stages wants, and each
//! stage scales that down for itself ([`Bitmap::fit`]).
//!
//! COM must be initialised on the calling thread ([`crate::Apartment`]). Callers check that a
//! file is on this computer before decoding it: opening a cloud-only file downloads it.

use windows::Win32::Foundation::{GENERIC_READ, WINCODEC_ERR_COMPONENTNOTFOUND};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    IWICBitmapSource, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE, MF_MT_VIDEO_ROTATION, MF_PD_DURATION, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
    MF_SOURCE_READERF_ENDOFSTREAM, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL,
    MFMediaType_Video, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantToUInt64};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, STREAM_SEEK_END, STREAM_SEEK_SET};
use windows::Win32::System::Variant::VT_I8;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{GUID, HSTRING, Interface};

/// A picture in memory: rows of blue, green, red and alpha bytes, top row first.
#[derive(Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bitmap({}x{})", self.width, self.height)
    }
}

/// Why something could not be decoded, in words for the library's settings page.
pub fn describe(error: &windows::core::Error) -> String {
    if error.code() == WINCODEC_ERR_COMPONENTNOTFOUND {
        return "Windows has no decoder for this format (it may need an extension from the Microsoft Store)".into();
    }
    let message = error.message();
    if message.trim().is_empty() { format!("Windows couldn't read it ({:#010x})", error.code().0) } else { message.trim().to_owned() }
}

fn factory() -> windows::core::Result<IWICImagingFactory> {
    // SAFETY: COM is initialised on this thread, as every function here requires.
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

/// The size that fits `width` by `height` within `max_side` on its longer side, never larger
/// than it is, and never zero.
pub fn fitted(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let longer = width.max(height);
    if longer <= max_side || longer == 0 {
        return (width.max(1), height.max(1));
    }
    let scale = f64::from(max_side) / f64::from(longer);
    let side = |n: u32| ((f64::from(n) * scale).round() as u32).clamp(1, max_side);
    (side(width), side(height))
}

/// Reads a source's pixels as BGRA.
///
/// # Safety
/// COM must be initialised on this thread.
unsafe fn pixels_of(factory: &IWICImagingFactory, source: &IWICBitmapSource) -> windows::core::Result<Bitmap> {
    // SAFETY: as the caller promises; the buffer is sized for the rows asked for.
    unsafe {
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(source, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut)?;
        let (mut width, mut height) = (0, 0);
        converter.GetSize(&mut width, &mut height)?;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels)?;
        Ok(Bitmap { width, height, pixels })
    }
}

/// Decodes the first frame of an image, scaled down to fit `max_side` (before it is turned) and
/// turned by its EXIF `orientation` (1 to 8) as it is shown.
pub fn image(path: &str, max_side: u32, orientation: u16) -> Result<Bitmap, String> {
    let decode = || -> windows::core::Result<Bitmap> {
        let factory = factory()?;
        // SAFETY: COM is initialised on this thread; every interface is released when dropped.
        unsafe {
            let decoder = factory.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
            let frame = decoder.GetFrame(0)?;
            let (mut width, mut height) = (0, 0);
            frame.GetSize(&mut width, &mut height)?;
            let (to_width, to_height) = fitted(width, height, max_side);
            let source: IWICBitmapSource = if (to_width, to_height) == (width, height) {
                frame.cast()?
            } else {
                // A JPEG decoder scales while it decodes when the scaler asks it to, so a large
                // photo is never decoded whole to make a small picture.
                let scaler = factory.CreateBitmapScaler()?;
                scaler.Initialize(&frame, to_width, to_height, WICBitmapInterpolationModeFant)?;
                scaler.cast()?
            };
            pixels_of(&factory, &source)
        }
    };
    decode().map(|bitmap| bitmap.oriented(orientation)).map_err(|error| describe(&error))
}

impl Bitmap {
    /// A bitmap of one colour, for tests.
    pub fn filled(width: u32, height: u32, bgra: [u8; 4]) -> Self {
        Bitmap { width, height, pixels: bgra.repeat(width as usize * height as usize) }
    }

    /// The pixel at a point, as BGRA.
    pub fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]]
    }

    /// The bitmap turned as EXIF orientation `orientation` says it is shown: 2 mirrored, 3
    /// turned half way, 4 flipped, 5 transposed, 6 turned a quarter clockwise, 7 transversed, 8
    /// turned a quarter anticlockwise. 1 and anything unknown leave it as it is.
    pub fn oriented(self, orientation: u16) -> Bitmap {
        if !(2..=8).contains(&orientation) {
            return self;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let turned = orientation >= 5;
        let (out_w, out_h) = if turned { (h, w) } else { (w, h) };
        let mut out = vec![0u8; self.pixels.len()];
        for y in 0..out_h {
            for x in 0..out_w {
                // Where the pixel shown at (x, y) is stored.
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    _ => (w - 1 - y, x),
                };
                let from = (sy * w + sx) * 4;
                let to = (y * out_w + x) * 4;
                out[to..to + 4].copy_from_slice(&self.pixels[from..from + 4]);
            }
        }
        Bitmap { width: out_w as u32, height: out_h as u32, pixels: out }
    }

    /// The bitmap scaled down to fit `max_side`, or a copy if it already does.
    pub fn fit(&self, max_side: u32) -> Result<Bitmap, String> {
        let (width, height) = fitted(self.width, self.height, max_side);
        if (width, height) == (self.width, self.height) {
            return Ok(self.clone());
        }
        let scale = || -> windows::core::Result<Bitmap> {
            let factory = factory()?;
            // SAFETY: COM is initialised on this thread; WIC copies the pixels it is given.
            unsafe {
                let source = factory.CreateBitmapFromMemory(self.width, self.height, &GUID_WICPixelFormat32bppBGRA, self.width * 4, &self.pixels)?;
                let scaler = factory.CreateBitmapScaler()?;
                scaler.Initialize(&source, width, height, WICBitmapInterpolationModeFant)?;
                pixels_of(&factory, &scaler.cast()?)
            }
        };
        scale().map_err(|error| describe(&error))
    }

    /// Encodes the bitmap as JPEG (WIC's default quality, 0.9), dropping any transparency.
    pub fn to_jpeg(&self) -> Result<Vec<u8>, String> {
        self.encode(&GUID_ContainerFormatJpeg)
    }

    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        self.encode(&GUID_ContainerFormatPng)
    }

    fn encode(&self, container: &GUID) -> Result<Vec<u8>, String> {
        let encode = || -> windows::core::Result<Vec<u8>> {
            let factory = factory()?;
            // SAFETY: COM is initialised on this thread; the stream is read back after the
            // encoder has committed, and every interface is released when dropped.
            unsafe {
                let stream = SHCreateMemStream(None).ok_or_else(windows::core::Error::from_thread)?;
                let encoder = factory.CreateEncoder(container, std::ptr::null())?;
                encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
                let mut frame = None;
                encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
                let frame = frame.ok_or_else(windows::core::Error::empty)?;
                frame.Initialize(None)?;
                frame.SetSize(self.width, self.height)?;
                let source = factory.CreateBitmapFromMemory(self.width, self.height, &GUID_WICPixelFormat32bppBGRA, self.width * 4, &self.pixels)?;
                // The encoder converts to the pixel format it writes.
                frame.WriteSource(&source, std::ptr::null())?;
                frame.Commit()?;
                encoder.Commit()?;
                let mut length = 0u64;
                stream.Seek(0, STREAM_SEEK_END, Some(&mut length))?;
                stream.Seek(0, STREAM_SEEK_SET, None)?;
                let mut bytes = vec![0u8; length as usize];
                let mut read = 0u32;
                stream.Read(bytes.as_mut_ptr().cast(), length as u32, Some(&mut read)).ok()?;
                bytes.truncate(read as usize);
                Ok(bytes)
            }
        };
        encode().map_err(|error| describe(&error))
    }
}

// ---------- video frames ----------

/// The EXIF orientation that shows a video's stored frames upright. Phones record upright video
/// sideways and say how far to turn it clockwise (`MF_MT_VIDEO_ROTATION`, from the file's
/// track matrix), which is how Windows' own thumbnails turn it: 90 degrees is EXIF's 6.
pub fn orientation_of_rotation(degrees: u32) -> u16 {
    match degrees {
        90 => 6,
        180 => 3,
        270 => 8,
        _ => 1,
    }
}

/// How far a video's first video stream asks to be turned, clockwise in degrees.
///
/// # Safety
/// COM and Media Foundation must be started on this thread.
unsafe fn rotation_of(reader: &IMFSourceReader) -> u32 {
    // SAFETY: as the caller promises; the native type is the file's own, before any conversion.
    unsafe {
        reader
            .GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0)
            .and_then(|native| native.GetUINT32(&MF_MT_VIDEO_ROTATION))
            .unwrap_or(0)
    }
}

/// A frame of a video and how long the video is.
pub struct Frame {
    pub bitmap: Bitmap,
    /// The video's length in milliseconds, when Media Foundation knows it.
    pub duration_ms: Option<u64>,
}

/// Decodes the frame `at` of the way through a video (0.0 is the start, 0.5 the middle),
/// scaled to fit `max_side` and turned as the video is shown. Media Foundation converts it to
/// RGB32.
pub fn video_frame(path: &str, at: f64, max_side: u32) -> Result<Frame, String> {
    crate::com::media_foundation()?;
    let read = || -> windows::core::Result<(Frame, u16)> {
        // SAFETY: COM and Media Foundation are started on this thread; the buffer is locked
        // only while it is copied, and every interface is released when dropped.
        unsafe {
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 1)?;
            let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
            // The source reader converts from whatever the decoder makes to RGB32.
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&HSTRING::from(path), &attributes)?;
            let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let orientation = orientation_of_rotation(rotation_of(&reader));
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(video, true)?;
            let wanted: IMFMediaType = MFCreateMediaType()?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(video, None, &wanted)?;
            let current = reader.GetCurrentMediaType(video)?;
            let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let (width, height) = ((size >> 32) as u32, size as u32);
            let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(width as i32 * 4);

            let duration = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .and_then(|value: PROPVARIANT| PropVariantToUInt64(&value).ok());
            if let Some(ticks) = duration.filter(|&ticks| ticks > 0 && at > 0.0) {
                let mut position = PROPVARIANT::default();
                // A position in 100-nanosecond units, as a VT_I8.
                (*position.Anonymous.Anonymous).vt = VT_I8;
                (*position.Anonymous.Anonymous).Anonymous.hVal = (ticks as f64 * at.clamp(0.0, 0.99)) as i64;
                // A file that can't seek is read from the start instead.
                let _ = reader.SetCurrentPosition(&GUID::zeroed(), &position);
            }

            let mut sample = None;
            for _ in 0..240 {
                let mut flags = 0u32;
                let mut stream = 0u32;
                let mut time = 0i64;
                reader.ReadSample(video, 0, Some(&mut stream), Some(&mut flags), Some(&mut time), Some(&mut sample))?;
                if sample.is_some() || flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
            }
            let sample = sample.ok_or_else(|| windows::core::Error::new(windows::core::HRESULT(0x80004005u32 as i32), "The video has no frame to show"))?;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut length = 0u32;
            buffer.Lock(&mut data, None, Some(&mut length))?;
            let row = width as usize * 4;
            let pitch = stride.unsigned_abs() as usize;
            let mut pixels = vec![0u8; row * height as usize];
            if pitch >= row && (length as usize) >= pitch * (height as usize - 1) + row {
                let bytes = std::slice::from_raw_parts(data, length as usize);
                for y in 0..height as usize {
                    // A negative stride means the rows are stored bottom first.
                    let from = if stride < 0 { (height as usize - 1 - y) * pitch } else { y * pitch };
                    pixels[y * row..(y + 1) * row].copy_from_slice(&bytes[from..from + row]);
                }
            }
            buffer.Unlock()?;
            // RGB32 leaves the fourth byte undefined; frames are opaque.
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
            Ok((Frame { bitmap: Bitmap { width, height, pixels }, duration_ms: duration.map(|ticks| ticks / 10_000) }, orientation))
        }
    };
    let (frame, orientation) = read().map_err(|error| describe(&error))?;
    // Turned after it is scaled, which the turn doesn't change, so less is copied.
    let bitmap = frame.bitmap.fit(max_side)?.oriented(orientation);
    Ok(Frame { bitmap, duration_ms: frame.duration_ms })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3 by 2 bitmap whose pixels are numbered 0 to 5 in their blue byte.
    fn numbered() -> Bitmap {
        Bitmap { width: 3, height: 2, pixels: (0..6u8).flat_map(|n| [n, 0, 0, 255]).collect() }
    }

    fn blues(bitmap: &Bitmap) -> Vec<u8> {
        bitmap.pixels.chunks_exact(4).map(|p| p[0]).collect()
    }

    #[test]
    fn orientations_turn_pixels_as_exif_says() {
        // Stored:  0 1 2
        //          3 4 5
        assert_eq!(blues(&numbered().oriented(1)), [0, 1, 2, 3, 4, 5]);
        assert_eq!(blues(&numbered().oriented(2)), [2, 1, 0, 5, 4, 3]);
        assert_eq!(blues(&numbered().oriented(3)), [5, 4, 3, 2, 1, 0]);
        assert_eq!(blues(&numbered().oriented(4)), [3, 4, 5, 0, 1, 2]);
        let transposed = numbered().oriented(5);
        assert_eq!((transposed.width, transposed.height), (2, 3));
        assert_eq!(blues(&transposed), [0, 3, 1, 4, 2, 5]);
        // A quarter turn clockwise: the left column becomes the top row.
        assert_eq!(blues(&numbered().oriented(6)), [3, 0, 4, 1, 5, 2]);
        assert_eq!(blues(&numbered().oriented(7)), [5, 2, 4, 1, 3, 0]);
        assert_eq!(blues(&numbered().oriented(8)), [2, 5, 1, 4, 0, 3]);
        assert_eq!(blues(&numbered().oriented(9)), [0, 1, 2, 3, 4, 5], "unknown values change nothing");
    }

    #[test]
    fn sizes_fit_their_longer_side() {
        assert_eq!(fitted(4000, 3000, 256), (256, 192));
        assert_eq!(fitted(3000, 4000, 256), (192, 256));
        assert_eq!(fitted(100, 50, 256), (100, 50), "never enlarged");
        assert_eq!(fitted(10_000, 1, 100), (100, 1), "never zero");
    }

    #[test]
    fn bitmaps_scale_and_encode_through_wic() {
        let _com = crate::Apartment::enter();
        let mut bitmap = Bitmap::filled(400, 200, [200, 100, 50, 255]);
        // The left half red, so scaling keeps sides apart.
        for y in 0..200 {
            for x in 0..200 {
                let i = (y * 400 + x) * 4;
                bitmap.pixels[i..i + 4].copy_from_slice(&[0, 0, 255, 255]);
            }
        }
        let small = bitmap.fit(100).unwrap();
        assert_eq!((small.width, small.height), (100, 50));
        assert_eq!(small.at(10, 25), [0, 0, 255, 255]);
        assert_eq!(small.at(90, 25), [200, 100, 50, 255]);
        let jpeg = small.to_jpeg().unwrap();
        assert_eq!(&jpeg[..2], [0xFF, 0xD8]);
        let png = small.to_png().unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }
}
