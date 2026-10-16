//! Pictures in memory, and decoding them from files with WIC, upright.
//!
//! WIC decodes every format Windows has a codec for (JPEG, PNG, GIF, BMP, TIFF, ICO, and HEIC,
//! WebP, AVIF and camera raw where their extensions are installed). The EXIF orientation is read
//! through WIC's metadata policy name and applied here, in plain code a test can check pixel by
//! pixel; alpha is dropped without blending, as the reference processor's RGB conversion does.

use std::path::Path;

use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICBitmapFrameDecode, IWICFormatConverter, IWICImagingFactory,
    IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_UI2;
use windows_core::HSTRING;

use crate::{Error, Result};

/// An 8-bit RGB picture, rows top to bottom, three bytes a pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgb {
    /// A picture of one colour.
    pub fn filled(width: u32, height: u32, colour: [u8; 3]) -> Rgb {
        Rgb { width, height, pixels: colour.iter().copied().cycle().take((width * height * 3) as usize).collect() }
    }

    pub fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * self.width + x) * 3) as usize;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2]]
    }

    pub fn set(&mut self, x: u32, y: u32, colour: [u8; 3]) {
        let i = ((y * self.width + x) * 3) as usize;
        self.pixels[i..i + 3].copy_from_slice(&colour);
    }

    /// Turns the picture upright from its EXIF orientation (1 to 8; anything else is 1).
    pub fn oriented(self, orientation: u16) -> Rgb {
        /// Where a pixel of the upright picture comes from in the stored one.
        type Source = Box<dyn Fn(u32, u32) -> (u32, u32)>;
        let (w, h) = (self.width, self.height);
        let (out_w, out_h, source): (u32, u32, Source) = match orientation {
            2 => (w, h, Box::new(move |x, y| (w - 1 - x, y))),
            3 => (w, h, Box::new(move |x, y| (w - 1 - x, h - 1 - y))),
            4 => (w, h, Box::new(move |x, y| (x, h - 1 - y))),
            // Transposed: stored rows are upright columns.
            5 => (h, w, Box::new(move |x, y| (y, x))),
            // Stored turned 90 degrees anticlockwise: turn it clockwise.
            6 => (h, w, Box::new(move |x, y| (y, h - 1 - x))),
            7 => (h, w, Box::new(move |x, y| (w - 1 - y, h - 1 - x))),
            8 => (h, w, Box::new(move |x, y| (w - 1 - y, x))),
            _ => return self,
        };
        let mut out = Rgb { width: out_w, height: out_h, pixels: vec![0; self.pixels.len()] };
        for y in 0..out_h {
            for x in 0..out_w {
                let (sx, sy) = source(x, y);
                out.set(x, y, self.at(sx, sy));
            }
        }
        out
    }
}

/// The largest picture Kestrel decodes, in pixels (a 200-megapixel panorama is more than any
/// camera makes and still fits in memory).
const MAX_PIXELS: u64 = 200_000_000;

fn factory() -> Result<IWICImagingFactory> {
    crate::com();
    // SAFETY: creating WIC's factory with no outer object; COM is usable on this thread.
    Ok(unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? })
}

/// The EXIF orientation a frame declares, 1 when it declares none.
fn orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    // SAFETY: the reader and the variant are owned here; the variant is read only after the call
    // filled it, and only as the type it says it holds.
    unsafe {
        let Ok(reader) = frame.GetMetadataQueryReader() else { return 1 };
        // The policy name covers JPEG's, TIFF's and HEIF's own places for the tag.
        for query in ["System.Photo.Orientation", "/app1/ifd/{ushort=274}", "/ifd/{ushort=274}"] {
            let mut value = PROPVARIANT::default();
            if reader.GetMetadataByName(&HSTRING::from(query), &mut value).is_ok() {
                let inner = &value.Anonymous.Anonymous;
                let found = (inner.vt == VT_UI2).then(|| inner.Anonymous.uiVal);
                let _ = PropVariantClear(&mut value);
                if let Some(found @ 1..=8) = found {
                    return found;
                }
            }
        }
        1
    }
}

/// The first frame of the picture at `path` as 8-bit RGBA (alpha left as it is, to be dropped),
/// its stored size and its EXIF orientation.
fn open(factory: &IWICImagingFactory, path: &Path) -> Result<(IWICFormatConverter, u32, u32, u16)> {
    let wide = HSTRING::from(path);
    // SAFETY: every interface is owned and released when dropped; the converter keeps the frame
    // (and so the decoder) alive.
    unsafe {
        let decoder = factory
            .CreateDecoderFromFilename(&wide, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .map_err(|error| Error(format!("Windows can't read {} as a picture: {}", path.display(), Error::from(error))))?;
        let frame = decoder.GetFrame(0)?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height)?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err(Error(format!("{} is {width} x {height} pixels", path.display())));
        }
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        Ok((converter, width, height, orientation(&frame)))
    }
}

/// Decodes the first frame of the picture at `path`, upright, as RGB.
pub fn decode(path: &Path) -> Result<Rgb> {
    let factory = factory()?;
    let (converter, width, height, orientation) = open(&factory, path)?;
    let stride = width * 4;
    let mut rgba = vec![0u8; (stride * height) as usize];
    // SAFETY: the buffer holds the whole frame at the stride given.
    unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut rgba)? };
    let pixels = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    Ok(Rgb { width, height, pixels }.oriented(orientation))
}

/// Bytes of decoded rows held at once by [`decode_resized`].
const STRIP_BYTES: usize = 4 << 20;

/// Decodes the picture at `path` upright and resized to `width` x `height` with Pillow's
/// bilinear filter: exactly `resize(&decode(path)?, width, height)`, the reference's
/// preprocessing, but a strip of rows at a time ([`crate::resize::Streaming`]), so a
/// 48-megapixel photo never sits in memory whole, let alone twice.
pub fn decode_resized(path: &Path, width: u32, height: u32) -> Result<Rgb> {
    let factory = factory()?;
    let (converter, stored_w, stored_h, orientation) = open(&factory, path)?;
    let mut streaming = crate::resize::Streaming::new(stored_w, stored_h, orientation, width, height)
        .ok_or_else(|| Error(format!("can't make a picture of {width} x {height}")))?;
    let row = stored_w as usize * 4;
    let strip = (STRIP_BYTES / row).max(1);
    let mut buffer = vec![0u8; strip.min(stored_h as usize) * row];
    let rows = streaming.stored_rows();
    let mut y = rows.start;
    while y < rows.end {
        let count = strip.min(rows.end - y);
        let rect = WICRect { X: 0, Y: y as i32, Width: stored_w as i32, Height: count as i32 };
        // SAFETY: the buffer holds `count` rows at the stride given.
        unsafe { converter.CopyPixels(&rect, row as u32, &mut buffer[..count * row])? };
        for (offset, pixels) in buffer[..count * row].chunks_exact(row).enumerate() {
            streaming.feed(y + offset, pixels);
        }
        y += count;
    }
    Ok(streaming.finish())
}

/// Writes `image` as a PNG (exact) or a JPEG (lossy) with WIC, for tests and fixtures.
pub fn encode(image: &Rgb, path: &Path, jpeg: bool) -> Result<()> {
    use windows::Win32::Foundation::GENERIC_WRITE;
    use windows::Win32::Graphics::Imaging::{
        GUID_ContainerFormatJpeg, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR, IWICBitmapFrameEncode,
        WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
    let factory = factory()?;
    let bgr: Vec<u8> = image.pixels.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0]]).collect();
    // SAFETY: owned interfaces; the pixel rows passed are exactly height x stride bytes.
    unsafe {
        let stream = factory.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path), GENERIC_WRITE.0)?;
        let container = if jpeg { GUID_ContainerFormatJpeg } else { GUID_ContainerFormatPng };
        let encoder = factory.CreateEncoder(&container, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        let mut options: Option<IPropertyBag2> = None;
        encoder.CreateNewFrame(&mut frame, &mut options)?;
        let frame = frame.ok_or_else(|| Error("WIC made no frame".into()))?;
        frame.Initialize(options.as_ref())?;
        frame.SetSize(image.width, image.height)?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut format)?;
        if format != GUID_WICPixelFormat24bppBGR {
            return Err(Error("the encoder wants another pixel format".into()));
        }
        frame.WritePixels(image.height, image.width * 3, &bgr)?;
        frame.Commit()?;
        encoder.Commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3 x 2 picture whose pixels are numbered: (10 * x + y, 0, 0).
    fn numbered() -> Rgb {
        let mut image = Rgb::filled(3, 2, [0, 0, 0]);
        for y in 0..2 {
            for x in 0..3 {
                image.set(x, y, [(10 * x + y) as u8, 0, 0]);
            }
        }
        image
    }

    fn rows(image: &Rgb) -> Vec<Vec<u8>> {
        (0..image.height).map(|y| (0..image.width).map(|x| image.at(x, y)[0]).collect()).collect()
    }

    /// Each EXIF orientation, worked out on paper: stored
    ///   00 10 20
    ///   01 11 21
    #[test]
    fn every_exif_orientation_turns_the_picture_upright() {
        let cases: [(u16, Vec<Vec<u8>>); 8] = [
            (1, vec![vec![0, 10, 20], vec![1, 11, 21]]),
            (2, vec![vec![20, 10, 0], vec![21, 11, 1]]),
            (3, vec![vec![21, 11, 1], vec![20, 10, 0]]),
            (4, vec![vec![1, 11, 21], vec![0, 10, 20]]),
            (5, vec![vec![0, 1], vec![10, 11], vec![20, 21]]),
            (6, vec![vec![1, 0], vec![11, 10], vec![21, 20]]),
            (7, vec![vec![21, 20], vec![11, 10], vec![1, 0]]),
            (8, vec![vec![20, 21], vec![10, 11], vec![0, 1]]),
        ];
        for (orientation, expected) in cases {
            assert_eq!(rows(&numbered().oriented(orientation)), expected, "orientation {orientation}");
        }
        assert_eq!(numbered().oriented(0), numbered(), "an unknown value changes nothing");
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-embed-image-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Quarters: red top left, green top right, blue bottom left, white bottom right.
    fn quarters(w: u32, h: u32) -> Rgb {
        let mut image = Rgb::filled(w, h, [0, 0, 0]);
        for y in 0..h {
            for x in 0..w {
                let colour = match (x < w / 2, y < h / 2) {
                    (true, true) => [255, 0, 0],
                    (false, true) => [0, 255, 0],
                    (true, false) => [0, 0, 255],
                    (false, false) => [255, 255, 255],
                };
                image.set(x, y, colour);
            }
        }
        image
    }

    /// An EXIF block (`APP1`) holding only an orientation, as cameras write it.
    fn exif_orientation(value: u16) -> Vec<u8> {
        let mut payload = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        payload.extend_from_slice(&value.to_le_bytes());
        payload.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut segment = vec![0xFF, 0xE1];
        segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        segment.extend(payload);
        segment
    }

    fn near(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 48)
    }

    /// A JPEG stored sideways with orientation 6 ("turn 90 degrees clockwise to view") comes out
    /// upright: blue top left, red top right, white bottom left, green bottom right.
    #[test]
    fn a_sideways_jpeg_is_decoded_upright() {
        let dir = scratch("exif");
        let plain = dir.join("plain.jpg");
        encode(&quarters(64, 32), &plain, true).unwrap();
        let bytes = std::fs::read(&plain).unwrap();
        assert_eq!(&bytes[..2], [0xFF, 0xD8]);
        let mut tagged = bytes[..2].to_vec();
        tagged.extend(exif_orientation(6));
        tagged.extend_from_slice(&bytes[2..]);
        let sideways = dir.join("sideways.jpg");
        std::fs::write(&sideways, tagged).unwrap();

        let upright = decode(&sideways).unwrap();
        assert_eq!((upright.width, upright.height), (32, 64));
        let at = |x: u32, y: u32| upright.at(x, y);
        assert!(near(at(8, 16), [0, 0, 255]) && near(at(24, 16), [255, 0, 0]), "{:?} {:?}", at(8, 16), at(24, 16));
        assert!(near(at(8, 48), [255, 255, 255]) && near(at(24, 48), [0, 255, 0]), "{:?} {:?}", at(8, 48), at(24, 48));
        // The same picture without the tag stays as stored.
        let stored = decode(&plain).unwrap();
        assert_eq!((stored.width, stored.height), (64, 32));
        assert!(near(stored.at(8, 8), [255, 0, 0]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// From a file to the model's input, by hand: a grey 4 x 1 PNG [0, 100, 200, 255] resized
    /// to 2 x 2 is [71, 209] on both rows (see `resize`), normalised (v / 255 - 0.5) / 0.5.
    #[test]
    fn a_png_becomes_the_tensor_worked_out_by_hand() {
        let dir = scratch("tensor");
        let path = dir.join("row.png");
        let row = Rgb { width: 4, height: 1, pixels: [0u8, 100, 200, 255].iter().flat_map(|&v| [v, v, v]).collect() };
        encode(&row, &path, false).unwrap();
        let decoded = decode(&path).unwrap();
        assert_eq!(decoded, row, "a PNG decodes exactly");
        let spec = crate::catalog::fast().image;
        let tensor = crate::preprocess::batch(&[decoded], 2, &spec);
        let (a, b) = ((71.0f32 / 255.0 - 0.5) / 0.5, (209.0f32 / 255.0 - 0.5) / 0.5);
        for plane in tensor.chunks_exact(4) {
            for (got, want) in plane.iter().zip([a, b, a, b]) {
                assert!((got - want).abs() < 1e-6, "{plane:?}");
            }
        }
        assert!((a - (-0.443_137_26)).abs() < 1e-6 && (b - 0.639_215_7).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_no_picture_is_an_error() {
        let dir = scratch("bad");
        let path = dir.join("bad.jpg");
        std::fs::write(&path, b"no picture here").unwrap();
        assert!(decode(&path).unwrap_err().0.contains("can't read"));
        assert!(decode_resized(&path, 16, 16).unwrap_err().0.contains("can't read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decoded straight to the model's square a strip at a time, a photo comes out exactly as
    /// the reference makes it from the whole picture, turned or not, larger than one strip or
    /// not.
    #[test]
    fn a_photo_decoded_to_its_size_is_the_whole_picture_resized() {
        let dir = scratch("resized");
        // Noise survives a PNG exactly; a strip is 4 MB, so 1200 x 1000 RGBA is several strips.
        let mut picture = quarters(1200, 1000);
        for (i, value) in picture.pixels.iter_mut().enumerate() {
            *value = value.wrapping_add((i.wrapping_mul(2_654_435_761) >> 13) as u8 & 0x3f);
        }
        let png = dir.join("noise.png");
        encode(&picture, &png, false).unwrap();
        let jpeg = dir.join("photo.jpg");
        encode(&picture, &jpeg, true).unwrap();
        let bytes = std::fs::read(&jpeg).unwrap();
        let mut tagged = bytes[..2].to_vec();
        tagged.extend(exif_orientation(6));
        tagged.extend_from_slice(&bytes[2..]);
        let turned = dir.join("turned.jpg");
        std::fs::write(&turned, tagged).unwrap();
        for path in [&png, &jpeg, &turned] {
            for (w, h) in [(256, 256), (384, 384), (1000, 1200)] {
                let reference = crate::resize::resize(&decode(path).unwrap(), w, h);
                assert_eq!(decode_resized(path, w, h).unwrap(), reference, "{} to {w} x {h}", path.display());
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
