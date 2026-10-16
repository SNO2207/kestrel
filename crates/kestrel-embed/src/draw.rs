//! Pictures drawn in memory with GDI, for the self-test and for test fixtures: plain colours,
//! and words in a system font, centred, one line under another.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FF_SWISS, FW_BOLD, FillRect, GdiFlush, HGDIOBJ, OUT_DEFAULT_PRECIS,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_core::w;

use crate::{Error, Result, Rgb};

fn colorref(colour: [u8; 3]) -> COLORREF {
    COLORREF(u32::from(colour[0]) | u32::from(colour[1]) << 8 | u32::from(colour[2]) << 16)
}

/// A `width` x `height` picture of `background` with `lines` of bold text in `ink`, each line
/// `font_px` pixels high, centred as a block.
pub fn words(width: u32, height: u32, background: [u8; 3], ink: [u8; 3], lines: &[&str], font_px: i32) -> Result<Rgb> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative: rows top to bottom.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: every GDI object made here is selected out (by deleting the DC first) and deleted
    // before returning; the pixel memory belongs to the DIB section, read after GdiFlush and
    // before the section is deleted, within the size the header gives it.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return Err(Error("GDI couldn't make a drawing surface".into()));
        }
        let mut bits = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) if !bits.is_null() => bitmap,
            _ => {
                let _ = DeleteDC(dc);
                return Err(Error("GDI couldn't make a bitmap".into()));
            }
        };
        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        let brush = CreateSolidBrush(colorref(background));
        let whole = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
        FillRect(dc, &whole, brush);
        let font = CreateFontW(
            -font_px,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
            w!("Arial"),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, colorref(ink));
        let line_height = font_px * 5 / 4;
        let top = height as i32 / 2 - line_height * lines.len() as i32 / 2;
        for (i, line) in lines.iter().enumerate() {
            let mut text: Vec<u16> = line.encode_utf16().collect();
            let y = top + line_height * i as i32;
            let mut rect = RECT { left: 0, top: y, right: width as i32, bottom: y + line_height };
            DrawTextW(dc, &mut text, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        }
        let _ = GdiFlush();
        let count = (width * height) as usize;
        let bgra = std::slice::from_raw_parts(bits as *const u8, count * 4);
        let pixels = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        SelectObject(dc, old_font);
        SelectObject(dc, old_bitmap);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        Ok(Rgb { width, height, pixels })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_drawn_in_ink_on_the_background() {
        let picture = words(200, 100, [255, 255, 255], [0, 0, 0], &["STOP"], 40).unwrap();
        assert_eq!((picture.width, picture.height, picture.pixels.len()), (200, 100, 60_000));
        assert_eq!(picture.at(0, 0), [255, 255, 255], "the corner is background");
        let inked = picture.pixels.chunks_exact(3).filter(|p| p[0] < 80).count();
        assert!(inked > 300 && inked < 8000, "{inked} dark pixels");
        // Red stays red: channels are not swapped.
        let red = words(10, 10, [200, 20, 10], [255, 255, 255], &[], 8).unwrap();
        assert_eq!(red.at(5, 5), [200, 20, 10]);
    }
}
