//! The date a photo was taken (`taken:` clauses), from the Windows property system, which reads
//! it from the file's EXIF, XMP or the container's own fields, whatever the format. The search
//! runs this only for files whose contents are on this PC (`kestrel_content::Run` never hands
//! it one that is only in the cloud).

use std::cell::Cell;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToFileTime};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::PSTF_UTC;
use windows::Win32::UI::Shell::PropertiesSystem::{GPS_DEFAULT, IPropertyStore, PSGetPropertyKeyFromName, SHGetPropertyStoreFromParsingName};
use windows::core::HSTRING;

thread_local! {
    /// COM is started once on each of the content search's threads, which live as long as the
    /// app does.
    static COM: Cell<bool> = const { Cell::new(false) };
}

fn ensure_com() {
    COM.with(|started| {
        if !started.get() {
            // SAFETY: initialises COM for this thread once; the thread keeps it until it ends.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            started.set(true);
        }
    });
}

/// When the photo at `path` was taken, as FILETIME ticks, or `None` if it doesn't say.
pub fn date_taken(path: &str) -> Option<i64> {
    ensure_com();
    // SAFETY: COM is initialised on this thread; the PROPVARIANT is cleared once read, and the
    // property store is released when dropped.
    unsafe {
        let mut key = PROPERTYKEY::default();
        PSGetPropertyKeyFromName(&HSTRING::from("System.Photo.DateTaken"), &mut key).ok()?;
        let store: IPropertyStore = SHGetPropertyStoreFromParsingName(&HSTRING::from(path), None, GPS_DEFAULT).ok()?;
        let mut value = store.GetValue(&key).ok()?;
        let time = PropVariantToFileTime(&value, PSTF_UTC).ok();
        let _ = PropVariantClear(&mut value);
        let time = time?;
        let ticks = (u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime)) as i64;
        (ticks > 0).then_some(ticks)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a small JPEG whose EXIF says it was taken at `when` (`2023:07:14 10:30:00`), with
    /// Windows PowerShell's System.Drawing, which every Windows has.
    pub(crate) fn photo_taken(path: &std::path::Path, when: &str) {
        let script = format!(
            "Add-Type -AssemblyName System.Drawing; \
             $b = New-Object System.Drawing.Bitmap 16,16; \
             $i = [System.Runtime.Serialization.FormatterServices]::GetUninitializedObject([System.Drawing.Imaging.PropertyItem]); \
             $i.Id = 0x9003; $i.Type = 2; $v = [System.Text.Encoding]::ASCII.GetBytes('{when}' + [char]0); $i.Len = $v.Length; $i.Value = $v; \
             $b.SetPropertyItem($i); $b.Save('{}', [System.Drawing.Imaging.ImageFormat]::Jpeg); $b.Dispose()",
            path.display()
        );
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .expect("Windows PowerShell runs");
        assert!(status.success() && path.exists(), "the photo was written");
    }

    #[test]
    fn the_date_a_photo_was_taken_is_read() {
        let dir = std::env::temp_dir().join(format!("kestrel-taken-photo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("beach.jpg");
        photo_taken(&photo, "2023:07:14 10:30:00");
        let ticks = date_taken(photo.to_str().unwrap()).expect("a date taken");
        // Local time in the file, UTC in the answer: within a day of the moment, whatever the zone.
        let unix = ticks / 10_000_000 - 11_644_473_600;
        let moment = 1_689_330_600; // 2023-07-14 10:30:00 UTC
        assert!((unix - moment).abs() <= 14 * 3600, "{unix}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_without_a_date_taken_have_none() {
        let dir = std::env::temp_dir().join(format!("kestrel-taken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("note.txt");
        std::fs::write(&text, b"no photo here").unwrap();
        assert_eq!(date_taken(text.to_str().unwrap()), None);
        assert_eq!(date_taken(dir.join("missing.jpg").to_str().unwrap()), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
