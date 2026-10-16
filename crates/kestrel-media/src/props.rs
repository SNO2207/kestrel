//! What the Windows property system knows of a photo or video (`IPropertyStore`): its
//! dimensions and orientation, when it was taken, the camera, the GPS position, and a video's
//! length, frame rate and codec. The property handlers read EXIF, XMP and container metadata,
//! so nothing here parses files itself.
//!
//! COM must be initialised on the calling thread ([`crate::Apartment`]). Getting a file's
//! property store opens it, so callers check first that it is on this computer.

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Storage::EnhancedStorage::{
    PKEY_GPS_Latitude, PKEY_GPS_LatitudeDecimal, PKEY_GPS_LatitudeRef, PKEY_GPS_Longitude, PKEY_GPS_LongitudeDecimal,
    PKEY_GPS_LongitudeRef, PKEY_Image_HorizontalSize, PKEY_Image_VerticalSize, PKEY_Media_DateEncoded, PKEY_Media_Duration,
    PKEY_Photo_CameraManufacturer, PKEY_Photo_CameraModel, PKEY_Photo_DateTaken, PKEY_Photo_Orientation, PKEY_Video_FourCC,
    PKEY_Video_FrameHeight, PKEY_Video_FrameRate, PKEY_Video_FrameWidth, PKEY_Video_Orientation,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PropVariantClear, PropVariantToDouble, PropVariantToDoubleVector, PropVariantToFileTime,
    PropVariantToStringAlloc, PropVariantToUInt16, PropVariantToUInt32, PropVariantToUInt64,
};
use windows::Win32::System::Variant::{PSTF_UTC, VT_EMPTY};
use windows::Win32::UI::Shell::PropertiesSystem::{GPS_DEFAULT, IPropertyStore, SHGetPropertyStoreFromParsingName};
use windows::core::HSTRING;

use crate::{Facts, Kind};

/// One property's value, cleared when dropped.
struct Value(PROPVARIANT);

impl Drop for Value {
    fn drop(&mut self) {
        // SAFETY: the value came from `GetValue` and is cleared once.
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}

impl Value {
    fn present(&self) -> bool {
        // SAFETY: reading the type tag of an initialised PROPVARIANT.
        unsafe { self.0.Anonymous.Anonymous.vt != VT_EMPTY }
    }

    fn u32(&self) -> Option<u32> {
        // SAFETY: the conversion reads the value by its type tag.
        self.present().then(|| unsafe { PropVariantToUInt32(&self.0) }.ok()).flatten()
    }

    fn u16(&self) -> Option<u16> {
        // SAFETY: as above.
        self.present().then(|| unsafe { PropVariantToUInt16(&self.0) }.ok()).flatten()
    }

    fn u64(&self) -> Option<u64> {
        // SAFETY: as above.
        self.present().then(|| unsafe { PropVariantToUInt64(&self.0) }.ok()).flatten()
    }

    fn f64(&self) -> Option<f64> {
        // SAFETY: as above.
        self.present().then(|| unsafe { PropVariantToDouble(&self.0) }.ok()).flatten().filter(|n| n.is_finite())
    }

    /// Degrees, minutes and seconds, as GPS latitudes and longitudes are stored.
    fn degrees(&self) -> Option<f64> {
        if !self.present() {
            return None;
        }
        let mut parts = [0f64; 3];
        let mut count = 0u32;
        // SAFETY: the buffer holds three values and the count says how many were written.
        unsafe { PropVariantToDoubleVector(&self.0, &mut parts, &mut count) }.ok()?;
        let parts = &parts[..count as usize];
        let degrees = parts.first()? + parts.get(1).unwrap_or(&0.0) / 60.0 + parts.get(2).unwrap_or(&0.0) / 3600.0;
        degrees.is_finite().then_some(degrees)
    }

    fn filetime(&self) -> Option<i64> {
        if !self.present() {
            return None;
        }
        // SAFETY: as above.
        let time = unsafe { PropVariantToFileTime(&self.0, PSTF_UTC) }.ok()?;
        let ticks = (time.dwHighDateTime as i64) << 32 | time.dwLowDateTime as i64;
        (ticks > 0).then_some(ticks)
    }

    fn string(&self) -> Option<String> {
        if !self.present() {
            return None;
        }
        // SAFETY: the string is allocated by the call and freed once here.
        unsafe {
            let text = PropVariantToStringAlloc(&self.0).ok()?;
            let copy = text.to_string().ok();
            CoTaskMemFree(Some(text.0 as *const _));
            copy.map(|text| text.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_owned()).filter(|text| !text.is_empty())
        }
    }
}

fn get(store: &IPropertyStore, key: &PROPERTYKEY) -> Value {
    // SAFETY: COM is initialised on this thread; a value that can't be read is empty.
    Value(unsafe { store.GetValue(key) }.unwrap_or_default())
}

/// A latitude or longitude in decimal degrees: Windows' own decimal property when it has one,
/// otherwise degrees, minutes and seconds with their hemisphere.
fn coordinate(store: &IPropertyStore, decimal: &PROPERTYKEY, parts: &PROPERTYKEY, hemisphere: &PROPERTYKEY, negative: char) -> Option<f64> {
    if let Some(value) = get(store, decimal).f64() {
        return Some(value);
    }
    let degrees = get(store, parts).degrees()?;
    let side = get(store, hemisphere).string().unwrap_or_default();
    Some(if side.starts_with(negative) || side.starts_with(negative.to_ascii_lowercase()) { -degrees } else { degrees })
}

/// A video codec's usual name, from the FourCC the property system reports.
pub fn codec_name(fourcc: u32) -> Option<String> {
    let bytes = fourcc.to_le_bytes();
    if bytes.iter().any(|&b| !(0x20..0x7f).contains(&b)) {
        return None;
    }
    let code = String::from_utf8_lossy(&bytes).trim().to_owned();
    let name = match code.to_ascii_lowercase().as_str() {
        "h264" | "avc1" | "x264" | "avc3" => "H.264",
        "hevc" | "hvc1" | "hev1" | "h265" => "HEVC",
        "av01" => "AV1",
        "vp90" | "vp09" => "VP9",
        "vp80" | "vp08" => "VP8",
        "mp4v" | "fmp4" | "xvid" | "divx" | "dx50" => "MPEG-4",
        "mpg2" | "mp2v" => "MPEG-2",
        "wmv3" => "WMV 9",
        "wvc1" => "VC-1",
        "mjpg" => "Motion JPEG",
        _ if code.is_empty() => return None,
        _ => return Some(code),
    };
    Some(name.to_owned())
}

/// Reads a file's facts. Dimensions come back as shown, turned by the orientation.
pub fn read(path: &str, kind: Kind) -> Result<Facts, String> {
    // SAFETY: COM is initialised on this thread, as this module requires.
    let store: IPropertyStore = unsafe { SHGetPropertyStoreFromParsingName(&HSTRING::from(path), None, GPS_DEFAULT) }
        .map_err(|error| crate::decode::describe(&error))?;
    let mut facts = Facts::default();
    match kind {
        Kind::Photo => {
            // Windows reports a photo's dimensions as it is shown, already turned by its
            // orientation (checked with a JPEG turned a quarter). The pipeline checks them
            // against the decoded picture all the same (`pipeline::agree`).
            facts.width = get(&store, &PKEY_Image_HorizontalSize).u32();
            facts.height = get(&store, &PKEY_Image_VerticalSize).u32();
            facts.orientation = get(&store, &PKEY_Photo_Orientation).u16().filter(|o| (1..=8).contains(o));
            facts.taken = get(&store, &PKEY_Photo_DateTaken).filetime();
            facts.make = get(&store, &PKEY_Photo_CameraManufacturer).string();
            facts.model = get(&store, &PKEY_Photo_CameraModel).string();
        }
        Kind::Video => {
            facts.width = get(&store, &PKEY_Video_FrameWidth).u32();
            facts.height = get(&store, &PKEY_Video_FrameHeight).u32();
            // Phones record upright video sideways and say how to turn it, in degrees.
            if matches!(get(&store, &PKEY_Video_Orientation).u32(), Some(90 | 270)) {
                std::mem::swap(&mut facts.width, &mut facts.height);
            }
            facts.taken = get(&store, &PKEY_Media_DateEncoded).filetime();
            facts.duration_ms = get(&store, &PKEY_Media_Duration).u64().map(|ticks| ticks / 10_000);
            // Frames per thousand seconds.
            facts.frame_rate = get(&store, &PKEY_Video_FrameRate).u32().filter(|&rate| rate > 0).map(|rate| f64::from(rate) / 1000.0);
            facts.codec = get(&store, &PKEY_Video_FourCC).u32().and_then(codec_name);
            facts.make = get(&store, &PKEY_Photo_CameraManufacturer).string();
            facts.model = get(&store, &PKEY_Photo_CameraModel).string();
        }
    }
    facts.width = facts.width.filter(|&n| n > 0);
    facts.height = facts.height.filter(|&n| n > 0);
    let latitude = coordinate(&store, &PKEY_GPS_LatitudeDecimal, &PKEY_GPS_Latitude, &PKEY_GPS_LatitudeRef, 'S');
    let longitude = coordinate(&store, &PKEY_GPS_LongitudeDecimal, &PKEY_GPS_Longitude, &PKEY_GPS_LongitudeRef, 'W');
    if let (Some(latitude), Some(longitude)) = (latitude, longitude)
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
        // Cameras without a fix often write zeroes.
        && (latitude, longitude) != (0.0, 0.0)
    {
        facts.latitude = Some(latitude);
        facts.longitude = Some(longitude);
    }
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_are_named() {
        let fourcc = |code: &[u8; 4]| u32::from_le_bytes(*code);
        assert_eq!(codec_name(fourcc(b"H264")).as_deref(), Some("H.264"));
        assert_eq!(codec_name(fourcc(b"avc1")).as_deref(), Some("H.264"));
        assert_eq!(codec_name(fourcc(b"hvc1")).as_deref(), Some("HEVC"));
        assert_eq!(codec_name(fourcc(b"ABCD")).as_deref(), Some("ABCD"));
        assert_eq!(codec_name(0), None);
    }
}
