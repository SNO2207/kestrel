//! Frames from videos, through Media Foundation's source reader, which decodes whatever Windows
//! has codecs for (H.264, HEVC with its extension, VP9, AV1...) with no ffmpeg.
//!
//! A video is sampled one frame every few seconds, at most so many per video (a longer video's
//! samples spread out), each at the moment it shows, so a search hit can open at that moment.
//! A seek lands on the key frame before the moment; when that is far from it, frames are
//! decoded forward to it. Where two samples in a row look very different (a cut), one more frame
//! is taken between them, which finds where the new scene starts to within half the interval
//! for the price of a single seek.
//!
//! [`write_test_video`] encodes coloured segments to an MP4 with Media Foundation's H.264
//! encoder, for tests and development builds.

use std::path::Path;
use std::sync::Once;

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantClear};
use windows::Win32::System::Variant::{VT_I8, VT_UI8};
use windows_core::{GUID, HSTRING, Interface};

use crate::{Error, Result, Rgb};

/// How to sample a video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// A frame every this many milliseconds, at the least.
    pub every_ms: u32,
    /// At most this many frames; a longer video's frames spread further apart.
    pub max_frames: usize,
    /// Take a frame between two samples that look very different.
    pub scene_changes: bool,
}

impl Default for Plan {
    fn default() -> Self {
        Plan { every_ms: 5000, max_frames: 24, scene_changes: true }
    }
}

/// What the video turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoInfo {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
}

/// Frames decoded forward from a key frame at most, looking for a moment.
const MAX_FORWARD: usize = 240;
/// How different two frames' 4 x 4 colour grids must be (mean absolute difference, 0 to 255)
/// to count as a cut.
const CUT: f32 = 40.0;

fn startup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        crate::com();
        // SAFETY: starts Media Foundation for the process; it is never shut down.
        let _ = unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) };
    });
}

fn time_variant(hundred_ns: i64) -> PROPVARIANT {
    PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_I8,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { hVal: hundred_ns },
            }),
        },
    }
}

/// A frame's colours on a 4 x 4 grid, to tell cuts.
fn signature(image: &Rgb) -> [f32; 48] {
    let mut sums = [0f32; 48];
    let mut counts = [0f32; 16];
    let (w, h) = (image.width.max(1), image.height.max(1));
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let cell = ((y * 4 / h) * 4 + x * 4 / w) as usize;
            let pixel = image.at(x, y);
            for c in 0..3 {
                sums[cell * 3 + c] += f32::from(pixel[c]);
            }
            counts[cell] += 1.0;
        }
    }
    std::array::from_fn(|i| sums[i] / counts[i / 3].max(1.0))
}

fn difference(a: &[f32; 48], b: &[f32; 48]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / 48.0
}

/// An open video, decoding to 32-bit RGB.
struct Reader {
    reader: IMFSourceReader,
    width: u32,
    height: u32,
    stride: i32,
}

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

impl Reader {
    fn open(path: &Path) -> Result<Reader> {
        startup();
        // SAFETY: Media Foundation calls on owned interfaces; every out value is read only after
        // its call succeeded.
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 2)?;
            let attributes = attributes.ok_or_else(|| Error("Media Foundation made no attributes".into()))?;
            // Lets the reader convert the decoder's YUV to RGB itself.
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path), &attributes)
                .map_err(|error| Error(format!("Windows can't read {} as a video: {}", path.display(), Error::from(error))))?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(VIDEO, true).map_err(|_| Error(format!("{} has no video", path.display())))?;
            let wanted = MFCreateMediaType()?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(VIDEO, None, &wanted)?;
            let mut opened = Reader { reader, width: 0, height: 0, stride: 0 };
            opened.read_format()?;
            Ok(opened)
        }
    }

    fn read_format(&mut self) -> Result<()> {
        // SAFETY: as in `open`.
        unsafe {
            let current = self.reader.GetCurrentMediaType(VIDEO)?;
            let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
            self.width = (size >> 32) as u32;
            self.height = size as u32;
            self.stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or((self.width * 4) as i32);
        }
        if self.width == 0 || self.height == 0 {
            return Err(Error("the video has no frame size".into()));
        }
        Ok(())
    }

    fn duration_ms(&self) -> u64 {
        // SAFETY: the attribute is a 64-bit count of 100-ns units when it is there; the variant
        // is cleared after it is read.
        unsafe {
            let Ok(mut value) = self.reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION) else {
                return 0;
            };
            let inner = &value.Anonymous.Anonymous;
            let duration = if inner.vt == VT_UI8 { inner.Anonymous.uhVal / 10_000 } else { 0 };
            let _ = PropVariantClear(&mut value);
            duration
        }
    }

    fn seek(&self, ms: u64) -> Result<()> {
        let mut position = time_variant(ms as i64 * 10_000);
        // SAFETY: the variant holds a plain 64-bit time; GUID_NULL is 100-ns units.
        let result = unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &position) };
        // SAFETY: clearing a VT_I8 variant frees nothing; it is never read again.
        let _ = unsafe { PropVariantClear(&mut position) };
        Ok(result?)
    }

    /// The next decoded frame and its time in milliseconds, or `None` at the end.
    fn next(&mut self) -> Result<Option<(u64, Rgb)>> {
        loop {
            let (mut flags, mut stamp) = (0u32, 0i64);
            let mut sample: Option<IMFSample> = None;
            // SAFETY: every out pointer is a live local.
            unsafe { self.reader.ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut stamp), Some(&mut sample))? };
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                self.read_format()?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
            let Some(sample) = sample else { continue };
            return Ok(Some(((stamp.max(0) / 10_000) as u64, self.pixels(&sample)?)));
        }
    }

    fn pixels(&self, sample: &IMFSample) -> Result<Rgb> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut pixels = vec![0u8; w * h * 3];
        // SAFETY: the buffer is locked while its bytes are read and unlocked after; rows are read
        // at the pitch the buffer reports (negative for bottom-up), within its length.
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let copy_rows = |start: *const u8, pitch: isize, pixels: &mut [u8]| {
                for y in 0..h {
                    let row = start.offset(pitch * y as isize);
                    for x in 0..w {
                        let p = row.add(x * 4);
                        let at = (y * w + x) * 3;
                        // BGRX in memory.
                        pixels[at] = *p.add(2);
                        pixels[at + 1] = *p.add(1);
                        pixels[at + 2] = *p;
                    }
                }
            };
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                let (mut scanline, mut pitch) = (std::ptr::null_mut(), 0i32);
                two_d.Lock2D(&mut scanline, &mut pitch)?;
                copy_rows(scanline, pitch as isize, &mut pixels);
                two_d.Unlock2D()?;
            } else {
                let (mut start, mut length) = (std::ptr::null_mut(), 0u32);
                buffer.Lock(&mut start, None, Some(&mut length))?;
                let pitch = self.stride as isize;
                if (length as usize) < w * h * 4 {
                    buffer.Unlock()?;
                    return Err(Error("a video frame was shorter than its size".into()));
                }
                // A negative default stride means the last row comes first.
                let first = if pitch < 0 { start.offset(-pitch * (h as isize - 1)) } else { start };
                copy_rows(first, pitch, &mut pixels);
                buffer.Unlock()?;
            }
        }
        Ok(Rgb { width: self.width, height: self.height, pixels })
    }

    /// The frame showing at `ms`: the key frame before it, or frames decoded forward from there
    /// when that key frame is more than `slack` earlier.
    fn frame_at(&mut self, ms: u64, slack: u64) -> Result<Option<(u64, Rgb)>> {
        self.seek(ms)?;
        let Some(mut found) = self.next()? else { return Ok(None) };
        for _ in 0..MAX_FORWARD {
            if found.0 + slack >= ms {
                break;
            }
            match self.next()? {
                Some(next) if next.0 <= ms => found = next,
                _ => break,
            }
        }
        Ok(Some(found))
    }
}

/// The moments to sample a video of `duration_ms` at.
pub fn moments(duration_ms: u64, plan: &Plan) -> Vec<u64> {
    let max = plan.max_frames.max(1) as u64;
    let every = u64::from(plan.every_ms.max(1)).max(duration_ms.div_ceil(max));
    let mut out: Vec<u64> = (0..max).map(|i| i * every).take_while(|&t| t < duration_ms.max(1)).collect();
    if out.is_empty() {
        out.push(0);
    }
    out
}

/// Samples the video at `path` by `plan`, handing each frame (its moment in milliseconds and its
/// pixels) to `on_frame` in time order, which says whether to go on.
pub fn sample(path: &Path, plan: &Plan, on_frame: &mut dyn FnMut(u32, Rgb) -> bool) -> Result<VideoInfo> {
    let mut reader = Reader::open(path)?;
    let duration_ms = reader.duration_ms();
    let wanted = moments(duration_ms, plan);
    let every = if wanted.len() > 1 { wanted[1] - wanted[0] } else { u64::from(plan.every_ms) };
    // A key frame up to a quarter of the interval (a second at most) early will do; phones
    // write one every second or two, so decoding forward from it costs little.
    let slack = (every / 4).min(1000);
    let mut frames = 0;
    let mut previous: Option<(u64, [f32; 48])> = None;
    for &at in &wanted {
        if frames >= plan.max_frames {
            break;
        }
        let Some((stamp, image)) = reader.frame_at(at, slack)? else { break };
        // A key frame already taken (sparse key frames and no frames between).
        if previous.is_some_and(|(last, _)| stamp <= last) {
            continue;
        }
        let sig = signature(&image);
        if plan.scene_changes
            && frames + 1 < plan.max_frames
            && let Some((last, last_sig)) = previous
            && difference(&last_sig, &sig) > CUT
            && stamp > last + 1
        {
            let middle = (last + stamp) / 2;
            if let Some((cut_stamp, cut_image)) = reader.frame_at(middle, 0)?
                && cut_stamp > last
                && cut_stamp < stamp
            {
                frames += 1;
                if !on_frame(cut_stamp as u32, cut_image) {
                    return Ok(VideoInfo { duration_ms, width: reader.width, height: reader.height, frames });
                }
            }
        }
        frames += 1;
        previous = Some((stamp, sig));
        if !on_frame(stamp as u32, image) {
            break;
        }
    }
    Ok(VideoInfo { duration_ms, width: reader.width, height: reader.height, frames })
}

/// Writes an H.264 MP4 of `width` x `height` at `fps`, showing each colour for its milliseconds.
pub fn write_test_video(path: &Path, width: u32, height: u32, fps: u32, segments: &[([u8; 3], u32)]) -> Result<()> {
    startup();
    let frame_ticks = 10_000_000 / i64::from(fps);
    // SAFETY: Media Foundation calls on owned interfaces; the sample buffer is locked while it is
    // filled, within the length it was made with.
    unsafe {
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path), None, None)?;
        let common = |media: &IMFMediaType, subtype: &GUID| -> windows_core::Result<()> {
            media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            media.SetUINT64(&MF_MT_FRAME_SIZE, u64::from(width) << 32 | u64::from(height))?;
            media.SetUINT64(&MF_MT_FRAME_RATE, u64::from(fps) << 32 | 1)?;
            media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)
        };
        let output = MFCreateMediaType()?;
        common(&output, &MFVideoFormat_H264)?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, 2_000_000)?;
        let stream = writer.AddStream(&output)?;
        let input = MFCreateMediaType()?;
        common(&input, &MFVideoFormat_RGB32)?;
        input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)?;
        writer.SetInputMediaType(stream, &input, None)?;
        writer.BeginWriting()?;
        let length = width * height * 4;
        let mut index = 0i64;
        for &(colour, ms) in segments {
            let count = (i64::from(ms) * i64::from(fps) / 1000).max(1);
            for _ in 0..count {
                let buffer = MFCreateMemoryBuffer(length)?;
                let mut start = std::ptr::null_mut();
                buffer.Lock(&mut start, None, None)?;
                let bytes = std::slice::from_raw_parts_mut(start, length as usize);
                for pixel in bytes.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[colour[2], colour[1], colour[0], 255]);
                }
                buffer.Unlock()?;
                buffer.SetCurrentLength(length)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                sample.SetSampleTime(index * frame_ticks)?;
                sample.SetSampleDuration(frame_ticks)?;
                writer.WriteSample(stream, &sample)?;
                index += 1;
            }
        }
        writer.Finalize()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moments_spread_out_over_long_videos() {
        let plan = Plan { every_ms: 5000, max_frames: 4, scene_changes: false };
        assert_eq!(moments(12_000, &plan), [0, 5000, 10_000]);
        assert_eq!(moments(100_000, &plan), [0, 25_000, 50_000, 75_000]);
        assert_eq!(moments(0, &plan), [0]);
        assert_eq!(moments(3000, &plan), [0]);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-embed-video-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn near(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 40)
    }

    /// Red for 3 s, then blue for 3 s, sampled every second: the frames show the colour of their
    /// moment, in order, upright, and the cut is found.
    #[test]
    fn frames_are_sampled_at_their_moments() {
        let dir = scratch("sample");
        let path = dir.join("red-then-blue.mp4");
        write_test_video(&path, 128, 96, 10, &[([220, 20, 20], 3000), ([20, 20, 220], 3000)]).unwrap();
        let mut frames = Vec::new();
        let info = sample(&path, &Plan { every_ms: 1000, max_frames: 10, scene_changes: false }, &mut |at, image| {
            frames.push((at, image.at(64, 48), image.width, image.height));
            true
        })
        .unwrap();
        assert!((5900..=6100).contains(&info.duration_ms), "{info:?}");
        assert_eq!((info.width, info.height), (128, 96));
        assert_eq!(frames.len(), 6, "{frames:?}");
        for (at, colour, w, h) in &frames {
            assert_eq!((*w, *h), (128, 96));
            let expected = if *at < 3000 { [220, 20, 20] } else { [20, 20, 220] };
            assert!(near(*colour, expected), "at {at} ms: {colour:?}");
        }
        assert!(frames.windows(2).all(|pair| pair[0].0 < pair[1].0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cut_between_samples_gets_a_frame_of_its_own() {
        let dir = scratch("cut");
        let path = dir.join("cut.mp4");
        // Green for 2.5 s, then red: sampled every 2 s, the cut falls between 2 s and 4 s.
        write_test_video(&path, 64, 64, 10, &[([20, 200, 20], 2500), ([220, 20, 20], 3500)]).unwrap();
        let mut moments = Vec::new();
        sample(&path, &Plan { every_ms: 2000, max_frames: 10, scene_changes: true }, &mut |at, image| {
            moments.push((at, image.at(32, 32)));
            true
        })
        .unwrap();
        // Three samples (a key frame up to 500 ms early may stand in for a moment), and one more
        // halfway between the two either side of the cut.
        let at: Vec<u32> = moments.iter().map(|m| m.0).collect();
        assert_eq!(at.len(), 4, "{at:?}");
        assert_eq!(at[0], 0);
        assert!((1500..=2000).contains(&at[1]) && (3500..=4000).contains(&at[3]), "{at:?}");
        assert!(at[2].abs_diff((at[1] + at[3]) / 2) <= 100, "the extra frame is halfway: {at:?}");
        assert!(near(moments[1].1, [20, 200, 20]) && near(moments[3].1, [220, 20, 20]), "{moments:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_no_video_is_an_error() {
        let dir = scratch("bad");
        let path = dir.join("bad.mp4");
        std::fs::write(&path, b"not a video at all").unwrap();
        assert!(sample(&path, &Plan::default(), &mut |_, _| true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
