//! Files for the tests, made here rather than found on the machine: JPEG photos carrying EXIF
//! (camera, date taken, orientation, GPS position), a short H.264 video written by Media
//! Foundation's sink writer, and images with text drawn into them by GDI.
//!
//! The unit tests use them, and so does the app's development hook for the end-to-end suite
//! (`media_dev_fixtures`), which has no other way to make a photo with EXIF or a video.
//!
//! COM must be initialised on the calling thread ([`crate::Apartment`]).

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSinkWriter, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_ROTATION, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Video, MFVideoFormat_H264,
    MFVideoFormat_RGB32, MFVideoInterlace_Progressive,
};
use windows::core::HSTRING;

use crate::decode::Bitmap;

/// What a fixture photo's EXIF says.
#[derive(Clone, Debug, Default)]
pub struct Exif {
    pub make: Option<String>,
    pub model: Option<String>,
    /// As EXIF writes it, in local time: `2024:07:14 10:30:00`.
    pub taken: Option<String>,
    pub orientation: Option<u16>,
    /// Latitude and longitude in decimal degrees.
    pub gps: Option<(f64, f64)>,
}

/// A bitmap in four quarters, so turning and flipping can be told apart: red at the top left,
/// green at the top right, blue at the bottom left and white at the bottom right.
pub fn quarters(width: u32, height: u32) -> Bitmap {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let colour = match (x < width / 2, y < height / 2) {
                (true, true) => [0, 0, 255, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [255, 0, 0, 255],
                (false, false) => [255, 255, 255, 255],
            };
            pixels.extend_from_slice(&colour);
        }
    }
    Bitmap { width, height, pixels }
}

/// Which quarter colour a BGRA pixel is nearest: `r`, `g`, `b` or `w`.
pub fn quarter_colour(pixel: [u8; 4]) -> char {
    let [b, g, r, _] = pixel.map(|c| c > 128);
    match (r, g, b) {
        (true, true, true) => 'w',
        (true, false, false) => 'r',
        (false, true, false) => 'g',
        (false, false, true) => 'b',
        _ => '?',
    }
}

// ---------- JPEG with EXIF ----------

/// One TIFF directory entry: a tag, a type and its value's bytes (little-endian).
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    bytes: Vec<u8>,
}

const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const RATIONAL: u16 = 5;
const BYTE: u16 = 1;

fn ascii(tag: u16, text: &str) -> Entry {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    Entry { tag, kind: ASCII, count: bytes.len() as u32, bytes }
}

fn rationals(tag: u16, values: &[(u32, u32)]) -> Entry {
    let bytes = values.iter().flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat()).collect();
    Entry { tag, kind: RATIONAL, count: values.len() as u32, bytes }
}

/// Degrees, minutes and seconds, as GPS coordinates are stored.
fn dms(value: f64) -> [(u32, u32); 3] {
    let value = value.abs();
    let degrees = value.floor();
    let minutes = ((value - degrees) * 60.0).floor();
    let seconds = ((value - degrees) * 60.0 - minutes) * 60.0;
    [(degrees as u32, 1), (minutes as u32, 1), ((seconds * 10_000.0).round() as u32, 10_000)]
}

/// The bytes an IFD takes at `offset`: its entries, then the values too large to sit in them.
fn ifd_size(entries: &[Entry]) -> usize {
    2 + entries.len() * 12 + 4 + entries.iter().map(|e| if e.bytes.len() > 4 { (e.bytes.len() + 1) & !1 } else { 0 }).sum::<usize>()
}

fn write_ifd(out: &mut Vec<u8>, mut entries: Vec<Entry>, offset: usize) {
    entries.sort_by_key(|e| e.tag);
    let mut data_at = offset + 2 + entries.len() * 12 + 4;
    let mut data = Vec::new();
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for entry in &entries {
        out.extend_from_slice(&entry.tag.to_le_bytes());
        out.extend_from_slice(&entry.kind.to_le_bytes());
        out.extend_from_slice(&entry.count.to_le_bytes());
        if entry.bytes.len() <= 4 {
            let mut inline = entry.bytes.clone();
            inline.resize(4, 0);
            out.extend_from_slice(&inline);
        } else {
            out.extend_from_slice(&(data_at as u32).to_le_bytes());
            data.extend_from_slice(&entry.bytes);
            if entry.bytes.len() % 2 == 1 {
                data.push(0);
            }
            data_at += (entry.bytes.len() + 1) & !1;
        }
    }
    // No next directory.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&data);
}

/// An EXIF APP1 segment saying what `exif` says.
pub fn exif_segment(exif: &Exif) -> Vec<u8> {
    let mut main = Vec::new();
    if let Some(make) = &exif.make {
        main.push(ascii(0x010F, make));
    }
    if let Some(model) = &exif.model {
        main.push(ascii(0x0110, model));
    }
    if let Some(orientation) = exif.orientation {
        main.push(Entry { tag: 0x0112, kind: SHORT, count: 1, bytes: orientation.to_le_bytes().to_vec() });
    }
    let photo: Vec<Entry> = exif.taken.iter().map(|taken| ascii(0x9003, taken)).collect();
    let gps: Vec<Entry> = match exif.gps {
        Some((latitude, longitude)) => vec![
            Entry { tag: 0x0000, kind: BYTE, count: 4, bytes: vec![2, 3, 0, 0] },
            ascii(0x0001, if latitude < 0.0 { "S" } else { "N" }),
            rationals(0x0002, &dms(latitude)),
            ascii(0x0003, if longitude < 0.0 { "W" } else { "E" }),
            rationals(0x0004, &dms(longitude)),
        ],
        None => Vec::new(),
    };
    // Pointers to the sub-directories are four bytes inline, so IFD0's size is known first.
    let pointers = usize::from(!photo.is_empty()) + usize::from(!gps.is_empty());
    let main_size = ifd_size(&main) + pointers * 12;
    let photo_at = 8 + main_size;
    let gps_at = photo_at + if photo.is_empty() { 0 } else { ifd_size(&photo) };
    if !photo.is_empty() {
        main.push(Entry { tag: 0x8769, kind: LONG, count: 1, bytes: (photo_at as u32).to_le_bytes().to_vec() });
    }
    if !gps.is_empty() {
        main.push(Entry { tag: 0x8825, kind: LONG, count: 1, bytes: (gps_at as u32).to_le_bytes().to_vec() });
    }
    let mut tiff = b"II*\0".to_vec();
    tiff.extend_from_slice(&8u32.to_le_bytes());
    write_ifd(&mut tiff, main, 8);
    if !photo.is_empty() {
        write_ifd(&mut tiff, photo, photo_at);
    }
    if !gps.is_empty() {
        write_ifd(&mut tiff, gps, gps_at);
    }
    let mut segment = vec![0xFF, 0xE1];
    segment.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
    segment.extend_from_slice(b"Exif\0\0");
    segment.extend_from_slice(&tiff);
    segment
}

/// Puts an APP1 segment into a JPEG, after its JFIF header if it has one.
pub fn with_segment(jpeg: &[u8], segment: &[u8]) -> Vec<u8> {
    let mut at = 2;
    if jpeg.len() > 4 && jpeg[2] == 0xFF && jpeg[3] == 0xE0 {
        at = 4 + u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize;
    }
    [&jpeg[..at], segment, &jpeg[at..]].concat()
}

/// Writes `bitmap` as a JPEG carrying `exif`.
pub fn write_jpeg(path: &Path, bitmap: &Bitmap, exif: &Exif) -> Result<(), String> {
    let jpeg = bitmap.to_jpeg()?;
    std::fs::write(path, with_segment(&jpeg, &exif_segment(exif))).map_err(|e| e.to_string())
}

// ---------- text ----------

/// A white picture with each of `lines` drawn on it in black, large and plain, as a screenshot
/// or a photographed receipt would show text.
pub fn text_bitmap(lines: &[&str], width: u32, height: u32) -> Result<Bitmap, String> {
    use windows_sys::Win32::Graphics::Gdi::{
        ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection,
        CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DeleteDC, DeleteObject, FW_SEMIBOLD, GdiFlush,
        OUT_DEFAULT_PRECIS, PatBlt, SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW, WHITENESS,
    };
    let line_height = (height / (lines.len() as u32 + 1)).min(72) as i32;
    // SAFETY: every GDI object made here is selected out and deleted before returning, and the
    // section's bits are read only while it exists and after GDI has finished drawing.
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return Err("GDI couldn't make a drawing surface".into());
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative: top row first.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let section = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if section.is_null() || bits.is_null() {
            DeleteDC(dc);
            return Err("GDI couldn't make a bitmap".into());
        }
        let previous = SelectObject(dc, section);
        PatBlt(dc, 0, 0, width as i32, height as i32, WHITENESS);
        let face: Vec<u16> = "Segoe UI".encode_utf16().chain([0]).collect();
        let font = CreateFontW(
            line_height * 3 / 4,
            0,
            0,
            0,
            FW_SEMIBOLD as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            ANTIALIASED_QUALITY as u32,
            DEFAULT_PITCH as u32,
            face.as_ptr(),
        );
        let previous_font = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0);
        for (n, line) in lines.iter().enumerate() {
            let text: Vec<u16> = line.encode_utf16().collect();
            TextOutW(dc, line_height / 2, line_height / 2 + n as i32 * line_height, text.as_ptr(), text.len() as i32);
        }
        GdiFlush();
        let length = width as usize * height as usize * 4;
        let mut pixels = std::slice::from_raw_parts(bits as *const u8, length).to_vec();
        SelectObject(dc, previous_font);
        DeleteObject(font);
        SelectObject(dc, previous);
        DeleteObject(section);
        DeleteDC(dc);
        // GDI leaves the alpha byte at zero.
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        Ok(Bitmap { width, height, pixels })
    }
}

// ---------- video ----------

/// Writes an H.264 MP4 of `frames` frames at `fps`, each from `frame(n)`, through Media
/// Foundation's sink writer (which converts the RGB32 frames for the encoder).
pub fn write_video(path: &Path, width: u32, height: u32, fps: u32, frames: u32, frame: impl Fn(u32) -> Bitmap) -> Result<(), String> {
    write_video_turned(path, width, height, fps, frames, 0, frame)
}

/// [`write_video`], stored with a rotation (degrees clockwise), as phones store upright video
/// sideways: a `width` x `height` video turned 90 degrees is shown `height` wide.
pub fn write_video_turned(path: &Path, width: u32, height: u32, fps: u32, frames: u32, rotation: u32, frame: impl Fn(u32) -> Bitmap) -> Result<(), String> {
    crate::com::media_foundation()?;
    let write = || -> windows::core::Result<()> {
        // SAFETY: COM and Media Foundation are started on this thread; each buffer is locked
        // only while it is filled, and every interface is released when dropped.
        unsafe {
            let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, None)?;
            let typed = |subtype| -> windows::core::Result<IMFMediaType> {
                let media: IMFMediaType = MFCreateMediaType()?;
                media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                media.SetUINT64(&MF_MT_FRAME_SIZE, u64::from(width) << 32 | u64::from(height))?;
                media.SetUINT64(&MF_MT_FRAME_RATE, u64::from(fps) << 32 | 1)?;
                media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)?;
                Ok(media)
            };
            let output = typed(&MFVideoFormat_H264)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, 1_000_000)?;
            if rotation != 0 {
                output.SetUINT32(&MF_MT_VIDEO_ROTATION, rotation)?;
            }
            let stream = writer.AddStream(&output)?;
            let input = typed(&MFVideoFormat_RGB32)?;
            // Positive: the rows given are top row first.
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)?;
            writer.SetInputMediaType(stream, &input, None)?;
            writer.BeginWriting()?;
            let length = width * height * 4;
            let duration = 10_000_000 / i64::from(fps);
            for n in 0..frames {
                let bitmap = frame(n);
                let buffer = MFCreateMemoryBuffer(length)?;
                let mut data = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None)?;
                std::ptr::copy_nonoverlapping(bitmap.pixels.as_ptr(), data, length as usize);
                buffer.Unlock()?;
                buffer.SetCurrentLength(length)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                sample.SetSampleTime(i64::from(n) * duration)?;
                sample.SetSampleDuration(duration)?;
                writer.WriteSample(stream, &sample)?;
            }
            writer.Finalize()
        }
    };
    write().map_err(|error| crate::decode::describe(&error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_segments_are_well_formed_tiff() {
        let exif = Exif { make: Some("Kestrel".into()), model: Some("Test Camera 1".into()), taken: Some("2024:07:14 10:30:00".into()), orientation: Some(6), gps: Some((51.5, -0.125)) };
        let segment = exif_segment(&exif);
        assert_eq!(&segment[..2], [0xFF, 0xE1]);
        assert_eq!(u16::from_be_bytes([segment[2], segment[3]]) as usize, segment.len() - 2);
        assert_eq!(&segment[4..10], b"Exif\0\0");
        let tiff = &segment[10..];
        assert_eq!(&tiff[..4], b"II*\0");
        // IFD0 has make, model, orientation and the two pointers, in tag order.
        assert_eq!(u16::from_le_bytes([tiff[8], tiff[9]]), 5);
        let tag = |at: usize| u16::from_le_bytes([tiff[at], tiff[at + 1]]);
        assert_eq!([tag(10), tag(22), tag(34), tag(46), tag(58)], [0x010F, 0x0110, 0x0112, 0x8769, 0x8825]);
        assert_eq!(dms(51.5), [(51, 1), (30, 1), (0, 10_000)]);
        assert_eq!(dms(-0.125), [(0, 1), (7, 1), (300_000, 10_000)]);
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0xAA, 0xBB, 0xFF, 0xD9];
        let spliced = with_segment(&jpeg, &[1, 2]);
        assert_eq!(spliced, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0xAA, 0xBB, 1, 2, 0xFF, 0xD9]);
    }

    #[test]
    fn quarters_are_told_apart() {
        let bitmap = quarters(4, 4);
        assert_eq!([bitmap.at(0, 0), bitmap.at(3, 0), bitmap.at(0, 3), bitmap.at(3, 3)].map(quarter_colour), ['r', 'g', 'b', 'w']);
    }
}
