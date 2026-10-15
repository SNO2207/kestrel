//! System file icons, rendered to PNG for the web view.
//!
//! All shell calls happen on one dedicated single-threaded-apartment thread, which is what the
//! shell icon APIs expect, and results are cached so an icon in use is rendered once. The cache
//! is bounded: the large views ask for a 256-pixel icon of every program, shortcut and icon file
//! they show, which over a session would otherwise add up to hundreds of megabytes.

use std::ffi::c_void;
use std::mem::zeroed;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::mpsc;

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, ReleaseDC,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::UI::Controls::{ILD_TRANSPARENT, ImageList_GetIcon};
use windows_sys::Win32::UI::Shell::{
    SHFILEINFOW, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetImageList,
    SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_LARGE, SHIL_SMALL,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows_sys::core::GUID;

use crate::lru::Lru;

const IID_IIMAGELIST: GUID = GUID {
    data1: 0x46EB5926,
    data2: 0x582E,
    data3: 0x4017,
    data4: [0x9F, 0xDF, 0xE8, 0x99, 0x8D, 0xAA, 0x09, 0x50],
};

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum IconKey {
    Folder,
    /// The generic icon for an extension, looked up without touching the disk.
    Ext(String),
    /// The icon of one specific file, for types such as `.exe` that carry their own.
    File(String),
}

pub type Png = Arc<Vec<u8>>;
type Reply = Box<dyn FnOnce(Option<Png>) + Send>;

/// Memory the icon cache may hold: a few hundred of the largest icons, and every small one.
const CACHE_CAP: usize = 32 << 20;
/// Memory counted for each cached icon besides its pixels, so that even the icons a file
/// doesn't have, which are remembered too, count towards the cap.
const ENTRY_OVERHEAD: usize = 64;

/// The shell's image list that holds icons of `size` pixels. Every size above 48 comes from
/// the 256-pixel list, so the thumbnail sizes, 64 to 1024, all share one rendering.
fn list_kind(size: u32) -> u32 {
    match size {
        0..=16 => SHIL_SMALL,
        17..=32 => SHIL_LARGE,
        33..=48 => SHIL_EXTRALARGE,
        // 256 pixels, for the large icon views.
        _ => SHIL_JUMBO,
    }
}

/// Rendered icons, by icon and image list, the least recently used going first once they add
/// up to the cap.
struct IconCache {
    lru: Lru<(IconKey, u32), Option<Png>>,
}

impl IconCache {
    fn new(cap: usize) -> Self {
        Self { lru: Lru::new(cap) }
    }

    /// The icon for `key` at `size` pixels, from the cache or else from `render`, which is
    /// given the key and the image list to render from.
    fn get_or_render(&mut self, key: IconKey, size: u32, render: impl FnOnce(&IconKey, u32) -> Option<Vec<u8>>) -> Option<Png> {
        let slot = (key, list_kind(size));
        if let Some(png) = self.lru.get(&slot) {
            return png;
        }
        let png = render(&slot.0, slot.1).map(Arc::new);
        let cost = png.as_ref().map_or(0, |png| png.len()) + ENTRY_OVERHEAD;
        self.lru.put(slot, png.clone(), cost);
        png
    }
}

pub struct IconService {
    requests: mpsc::Sender<(IconKey, u32, Reply)>,
}

impl IconService {
    pub fn start() -> Self {
        let (requests, inbox) = mpsc::channel::<(IconKey, u32, Reply)>();
        std::thread::Builder::new()
            .name("kestrel-icons".into())
            .spawn(move || {
                // SAFETY: initialises COM for this thread only; the thread lives for the process.
                unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED as u32) };
                let mut cache = IconCache::new(CACHE_CAP);
                for (key, size, reply) in inbox {
                    // SAFETY: called on the COM-initialised icon thread.
                    reply(cache.get_or_render(key, size, |key, list| unsafe { render(key, list) }));
                }
            })
            .expect("spawning the icon thread");
        Self { requests }
    }

    /// Renders the icon in the background and passes it to `reply`, or `None` if there is none.
    pub fn request(&self, key: IconKey, size: u32, reply: impl FnOnce(Option<Png>) + Send + 'static) {
        let _ = self.requests.send((key, size, Box::new(reply)));
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Releases a COM interface pointer through its IUnknown vtable.
unsafe fn release(unknown: *mut c_void) {
    type Release = unsafe extern "system" fn(*mut c_void) -> u32;
    // SAFETY: every COM object starts with a pointer to a vtable whose third slot is Release.
    unsafe {
        let vtable = *(unknown as *const *const Release);
        (*vtable.add(2))(unknown);
    }
}

/// Renders `key` from the image list `list_kind` (see [`list_kind`]) as PNG.
///
/// # Safety
/// Call on a thread where COM is initialised as a single-threaded apartment.
unsafe fn render(key: &IconKey, list_kind: u32) -> Option<Vec<u8>> {
    let (path, attributes, flags) = match key {
        IconKey::Folder => ("folder".to_owned(), FILE_ATTRIBUTE_DIRECTORY, SHGFI_USEFILEATTRIBUTES),
        IconKey::Ext(ext) => (format!(".{ext}"), FILE_ATTRIBUTE_NORMAL, SHGFI_USEFILEATTRIBUTES),
        IconKey::File(path) => (path.clone(), 0, 0),
    };
    // SAFETY: all pointers passed below are valid for the duration of each call, and every
    // handle or interface obtained is released before returning.
    unsafe {
        let mut info: SHFILEINFOW = zeroed();
        let found = SHGetFileInfoW(
            wide(&path).as_ptr(),
            attributes,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX | flags,
        );
        if found == 0 {
            return None;
        }
        let mut list: *mut c_void = null_mut();
        if SHGetImageList(list_kind as i32, &IID_IIMAGELIST, &mut list) < 0 || list.is_null() {
            return None;
        }
        // An IImageList pointer is interchangeable with an HIMAGELIST.
        let icon = ImageList_GetIcon(list as _, info.iIcon, ILD_TRANSPARENT);
        release(list);
        if icon.is_null() {
            return None;
        }
        let image = icon_pixels(icon);
        DestroyIcon(icon);
        let (width, height, rgba) = image?;
        if list_kind == SHIL_JUMBO
            && let Some(small) = shrink_small_jumbo(width, height, &rgba)
        {
            return encode_png(SMALL_JUMBO, SMALL_JUMBO, &small);
        }
        encode_png(width, height, &rgba)
    }
}

/// The largest icon size many older programs provide.
const SMALL_JUMBO: u32 = 48;

/// Types whose icons stop at 48 pixels appear in the 256-pixel list as a small icon in the
/// top-left corner of an empty square. Returns just that corner, so the view centres it.
fn shrink_small_jumbo(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if width <= SMALL_JUMBO || height <= SMALL_JUMBO {
        return None;
    }
    let row_len = width as usize * 4;
    let outside_corner = rgba.chunks_exact(row_len).enumerate().any(|(y, row)| {
        row.chunks_exact(4).enumerate().any(|(x, pixel)| {
            pixel[3] != 0 && (x as u32 >= SMALL_JUMBO || y as u32 >= SMALL_JUMBO)
        })
    });
    if outside_corner {
        return None;
    }
    let corner = SMALL_JUMBO as usize * 4;
    Some(rgba.chunks_exact(row_len).take(SMALL_JUMBO as usize).flat_map(|row| &row[..corner]).copied().collect())
}

/// Reads an icon's pixels as straight-alpha RGBA.
unsafe fn icon_pixels(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: `icon` is a valid icon handle; the bitmaps it yields are deleted below.
    unsafe {
        let mut info: ICONINFO = zeroed();
        if GetIconInfo(icon, &mut info) == 0 {
            return None;
        }
        let mut bitmap: BITMAP = zeroed();
        GetObjectW(info.hbmColor, size_of::<BITMAP>() as i32, (&raw mut bitmap).cast());
        let (width, height) = (bitmap.bmWidth, bitmap.bmHeight);

        let mut header: BITMAPINFO = zeroed();
        header.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative height requests top-down rows.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..zeroed()
        };
        let byte_len = (width.max(0) * height.max(0) * 4) as usize;
        let mut pixels = vec![0u8; byte_len];
        let dc = GetDC(null_mut());
        let mut read = |bitmap, target: &mut Vec<u8>| {
            GetDIBits(dc, bitmap, 0, height as u32, target.as_mut_ptr().cast(), &mut header, DIB_RGB_COLORS)
        };
        let ok = byte_len > 0 && read(info.hbmColor, &mut pixels) != 0;

        // Icons without an alpha channel carry transparency in a separate mask bitmap.
        if ok && pixels.chunks_exact(4).all(|p| p[3] == 0) {
            let mut mask = vec![0u8; byte_len];
            read(info.hbmMask, &mut mask);
            for (pixel, masked) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                pixel[3] = if masked[0] == 0 { 255 } else { 0 };
            }
        }
        ReleaseDC(null_mut(), dc);
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);

        // GDI returns BGRA.
        pixels.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
        ok.then_some((width as u32, height as u32, pixels))
    }
}

pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 256-pixel square with one opaque pixel at (x, y).
    fn jumbo_with_pixel_at(x: usize, y: usize) -> Vec<u8> {
        let mut rgba = vec![0u8; 256 * 256 * 4];
        rgba[(y * 256 + x) * 4..][..4].copy_from_slice(&[1, 2, 3, 255]);
        rgba
    }

    #[test]
    fn the_icon_cache_stays_within_its_cap() {
        let rendered = std::cell::Cell::new(0);
        let render = |_: &IconKey, list: u32| {
            rendered.set(rendered.get() + 1);
            // About what a 256-pixel icon takes as PNG.
            Some(vec![0; if list == SHIL_JUMBO { 40_000 } else { 2_000 }])
        };
        let mut cache = IconCache::new(1 << 20);
        // A large icon view scrolled through a folder of five thousand programs.
        for n in 0..5000 {
            cache.get_or_render(IconKey::File(format!(r"C:\Tools\tool{n}.exe")), 256, render);
        }
        assert!(cache.lru.used() <= 1 << 20, "{} bytes kept", cache.lru.used());
        assert!(cache.lru.len() < 30, "{} icons kept", cache.lru.len());

        // Icons in use stay; and the thumbnail sizes, which all come from the 256-pixel list,
        // share one rendering.
        let before = rendered.get();
        for size in [64, 96, 128, 256, 512, 1024] {
            assert_eq!(cache.get_or_render(IconKey::Ext("png".into()), size, render).map(|png| png.len()), Some(40_000));
        }
        assert_eq!(cache.get_or_render(IconKey::Ext("png".into()), 16, render).map(|png| png.len()), Some(2_000));
        assert_eq!(rendered.get() - before, 2, "one rendering per image list");
        assert!(cache.get_or_render(IconKey::Ext("png".into()), 128, |_, _| panic!("rendered again")).is_some());
    }

    #[test]
    fn small_icons_in_the_jumbo_list_are_cut_out() {
        let small = shrink_small_jumbo(256, 256, &jumbo_with_pixel_at(47, 10)).unwrap();
        assert_eq!(small.len(), 48 * 48 * 4);
        assert_eq!(&small[(10 * 48 + 47) * 4..][..4], [1, 2, 3, 255]);
        assert!(shrink_small_jumbo(256, 256, &jumbo_with_pixel_at(48, 10)).is_none(), "a real jumbo icon stays");
        assert!(shrink_small_jumbo(256, 256, &jumbo_with_pixel_at(3, 200)).is_none());
        assert!(shrink_small_jumbo(48, 48, &vec![0; 48 * 48 * 4]).is_none());
    }
}
