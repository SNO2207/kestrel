//! System file icons, rendered to PNG for the web view.
//!
//! All shell calls happen on one dedicated single-threaded-apartment thread, which is what the
//! shell icon APIs expect, and results are cached so each distinct icon is rendered once.

use std::collections::HashMap;
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
    SHIL_EXTRALARGE, SHIL_LARGE, SHIL_SMALL,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows_sys::core::GUID;

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
                let mut cache: HashMap<(IconKey, u32), Option<Png>> = HashMap::new();
                for (key, size, reply) in inbox {
                    let png = cache
                        .entry((key, size))
                        // SAFETY: called on the COM-initialised icon thread.
                        .or_insert_with_key(|(key, size)| unsafe { render(key, *size) }.map(Arc::new))
                        .clone();
                    reply(png);
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

unsafe fn render(key: &IconKey, size: u32) -> Option<Vec<u8>> {
    let (path, attributes, flags) = match key {
        IconKey::Folder => ("folder".to_owned(), FILE_ATTRIBUTE_DIRECTORY, SHGFI_USEFILEATTRIBUTES),
        IconKey::Ext(ext) => (format!(".{ext}"), FILE_ATTRIBUTE_NORMAL, SHGFI_USEFILEATTRIBUTES),
        IconKey::File(path) => (path.clone(), 0, 0),
    };
    let list_kind = match size {
        0..=16 => SHIL_SMALL,
        17..=32 => SHIL_LARGE,
        _ => SHIL_EXTRALARGE,
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
        encode_png(width, height, &rgba)
    }
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

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
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
