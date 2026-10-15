//! Reading the MFT: from the volume itself, which needs administrator rights, or from a dump
//! of it taken earlier, which lets the parsing be examined without them.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use rayon::prelude::*;

use crate::mft::{Record, apply_fixups, parse_record};

/// Where the MFT's bytes live and how they are divided into records.
#[derive(Clone, Debug)]
pub struct MftLayout {
    pub record_size: usize,
    pub cluster_size: u64,
    /// Bytes of the MFT in use; records beyond this have never been written.
    pub valid_bytes: u64,
    /// Contiguous pieces of the MFT, in order.
    pub extents: Vec<Extent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    /// Offset within the MFT.
    pub mft_offset: u64,
    /// Offset within the source being read.
    pub source_offset: u64,
    pub length: u64,
}

/// Something MFT bytes can be read from at arbitrary offsets.
pub trait Source: Sync {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()>;
}

impl Source for File {
    fn read_at(&self, mut offset: u64, mut buffer: &mut [u8]) -> io::Result<()> {
        #[cfg(windows)]
        use std::os::windows::fs::FileExt;
        while !buffer.is_empty() {
            #[cfg(windows)]
            let read = self.seek_read(buffer, offset)?;
            #[cfg(not(windows))]
            let read = std::os::unix::fs::FileExt::read_at(self, buffer, offset)?;
            if read == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the MFT ended early"));
            }
            buffer = &mut buffer[read..];
            offset += read as u64;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScanStats {
    /// Records read, in use or not.
    pub records: u64,
    /// Records in use that parsed.
    pub parsed: u64,
    /// Records whose multi-sector protection did not check out, usually mid-write.
    pub torn: u64,
}

/// Records read and parsed per batch: large enough for long sequential reads, small enough
/// to keep memory modest.
const CHUNK_RECORDS: usize = 32 * 1024;

/// Reads every record of the MFT, parsing in parallel, and passes each batch of in-use
/// records to `each` in MFT order.
pub fn scan(source: &impl Source, layout: &MftLayout, mut each: impl FnMut(Vec<Record>)) -> io::Result<ScanStats> {
    let mut stats = ScanStats::default();
    let record_size = layout.record_size;
    let mut buffer = vec![0u8; CHUNK_RECORDS * record_size];
    for extent in &layout.extents {
        let end = (extent.mft_offset + extent.length).min(layout.valid_bytes);
        let mut offset = extent.mft_offset;
        while offset < end {
            let length = ((end - offset) as usize).min(buffer.len()) / record_size * record_size;
            if length == 0 {
                break;
            }
            let chunk = &mut buffer[..length];
            source.read_at(extent.source_offset + (offset - extent.mft_offset), chunk)?;
            let first = offset / record_size as u64;
            let parsed: Vec<(bool, Option<Record>)> = chunk
                .par_chunks_exact_mut(record_size)
                .enumerate()
                .map(|(i, bytes)| {
                    if &bytes[..4] != b"FILE" {
                        return (false, None);
                    }
                    if !apply_fixups(bytes) {
                        return (true, None);
                    }
                    (false, parse_record(bytes, first + i as u64, false))
                })
                .collect();
            stats.records += (length / record_size) as u64;
            stats.torn += parsed.iter().filter(|(torn, _)| *torn).count() as u64;
            let records: Vec<Record> = parsed.into_iter().filter_map(|(_, record)| record).collect();
            stats.parsed += records.len() as u64;
            each(records);
            offset += length as u64;
        }
    }
    Ok(stats)
}

const DUMP_MAGIC: &[u8; 8] = b"KSTRLMFT";
const DUMP_HEADER: u64 = 32;

/// Copies the MFT to a file, so its parsing can be examined later without elevation.
pub fn write_dump(source: &impl Source, layout: &MftLayout, path: &Path) -> io::Result<()> {
    let mut out = io::BufWriter::with_capacity(1 << 22, File::create(path)?);
    out.write_all(DUMP_MAGIC)?;
    out.write_all(&(layout.record_size as u64).to_le_bytes())?;
    out.write_all(&layout.cluster_size.to_le_bytes())?;
    out.write_all(&layout.valid_bytes.to_le_bytes())?;
    let mut buffer = vec![0u8; 16 << 20];
    for extent in &layout.extents {
        let end = (extent.mft_offset + extent.length).min(layout.valid_bytes);
        let mut offset = extent.mft_offset;
        while offset < end {
            let length = ((end - offset) as usize).min(buffer.len());
            source.read_at(extent.source_offset + (offset - extent.mft_offset), &mut buffer[..length])?;
            out.write_all(&buffer[..length])?;
            offset += length as u64;
        }
    }
    out.into_inner()?.sync_all()
}

/// Opens a dump written by [`write_dump`].
pub fn open_dump(path: &Path) -> io::Result<(File, MftLayout)> {
    let mut file = File::open(path)?;
    let mut header = [0u8; DUMP_HEADER as usize];
    file.read_exact(&mut header)?;
    if &header[..8] != DUMP_MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not an MFT dump"));
    }
    let field = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
    let (record_size, cluster_size, valid_bytes) = (field(8) as usize, field(16), field(24));
    let layout = MftLayout {
        record_size,
        cluster_size,
        valid_bytes,
        extents: vec![Extent { mft_offset: 0, source_offset: DUMP_HEADER, length: valid_bytes }],
    };
    Ok((file, layout))
}

#[cfg(windows)]
pub use platform::*;

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;

    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};

    use crate::mft::Run;

    /// A volume opened for raw reading. Requires administrator rights.
    pub struct Volume {
        file: File,
        layout: MftLayout,
    }

    impl Source for Volume {
        fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
            self.file.read_at(offset, buffer)
        }
    }

    impl Volume {
        /// Opens the volume whose root is `root`, such as `C:\`, and locates its MFT.
        pub fn open(root: &str) -> io::Result<Self> {
            let letter = root.trim_end_matches(['\\', '/']);
            const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(SHARE_READ_WRITE)
                .open(format!(r"\\.\{letter}"))?;

            let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
            let mut returned = 0u32;
            // SAFETY: `data` is a valid out-buffer of the size passed.
            let ok = unsafe {
                DeviceIoControl(
                    file.as_raw_handle(),
                    FSCTL_GET_NTFS_VOLUME_DATA,
                    std::ptr::null(),
                    0,
                    (&raw mut data).cast(),
                    size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                    &mut returned,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let record_size = data.BytesPerFileRecordSegment as usize;
            let cluster_size = data.BytesPerCluster as u64;
            let valid_bytes = data.MftValidDataLength as u64;
            let mft_start = data.MftStartLcn as u64 * cluster_size;

            // Record 0 is the MFT's own record; its data runs say where the rest of it lives.
            let read_record = |file: &File, offset: u64| -> io::Result<Record> {
                let mut bytes = vec![0u8; record_size];
                file.read_at(offset, &mut bytes)?;
                if !apply_fixups(&mut bytes) {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "the MFT's own record is unreadable"));
                }
                parse_record(&bytes, 0, true).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the MFT's own record is invalid"))
            };
            let own = read_record(&file, mft_start)?;
            let mut pieces: Vec<(u64, Vec<Run>)> = own.runs.into_iter().collect();
            let mut layout = MftLayout { record_size, cluster_size, valid_bytes, extents: extents_of(&pieces, cluster_size) };

            // A heavily fragmented MFT keeps the rest of its run list in further records.
            for number in own.data_elsewhere {
                let offset = locate(&layout, number * record_size as u64)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "an MFT extension record lies outside the known MFT"))?;
                let mut record = read_record(&file, offset)?;
                record.number = number;
                pieces.extend(record.runs);
                layout.extents = extents_of(&pieces, cluster_size);
            }
            let covered: u64 = layout.extents.iter().map(|e| e.length).sum();
            if covered < valid_bytes {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "could not locate the whole MFT"));
            }
            Ok(Self { file, layout })
        }

        pub fn layout(&self) -> &MftLayout {
            &self.layout
        }
    }

    /// Turns run lists, each tagged with the first cluster of the stream it covers, into
    /// byte extents in MFT order.
    fn extents_of(pieces: &[(u64, Vec<Run>)], cluster_size: u64) -> Vec<Extent> {
        let mut sorted: Vec<&(u64, Vec<Run>)> = pieces.iter().collect();
        sorted.sort_by_key(|(vcn, _)| *vcn);
        let mut extents = Vec::new();
        for (first_vcn, runs) in sorted {
            let mut vcn = *first_vcn;
            for run in runs {
                if let Some(lcn) = run.lcn {
                    extents.push(Extent {
                        mft_offset: vcn * cluster_size,
                        source_offset: lcn as u64 * cluster_size,
                        length: run.clusters * cluster_size,
                    });
                }
                vcn += run.clusters;
            }
        }
        extents
    }

    /// Where on the volume the byte at `mft_offset` of the MFT lives.
    fn locate(layout: &MftLayout, mft_offset: u64) -> Option<u64> {
        layout
            .extents
            .iter()
            .find(|e| (e.mft_offset..e.mft_offset + e.length).contains(&mft_offset))
            .map(|e| e.source_offset + (mft_offset - e.mft_offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mft::testing::RecordBuilder;

    /// An in-memory MFT laid out in two extents.
    struct Memory(Vec<u8>);

    impl Source for Memory {
        fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
            let start = offset as usize;
            buffer.copy_from_slice(&self.0[start..start + buffer.len()]);
            Ok(())
        }
    }

    #[test]
    fn scans_records_across_extents_and_counts_torn_ones() {
        let record = |_number: u64, name: &str| {
            RecordBuilder::new(1024, 0x1, 1, 0).standard_info(1, 0x20).file_name(0x0005_0000_0000_0005, name, 1).finish()
        };
        // MFT records 0..4, stored as records 2..4 at the start of the source and 0..1 after.
        let mut source = Vec::new();
        source.extend(record(2, "two"));
        source.extend(vec![0u8; 1024]); // record 3: never written
        let mut torn = record(4, "four");
        torn[1022] ^= 1;
        source.extend(torn);
        source.extend(record(0, "zero"));
        source.extend(record(1, "one"));
        let layout = MftLayout {
            record_size: 1024,
            cluster_size: 1024,
            valid_bytes: 5 * 1024,
            extents: vec![
                Extent { mft_offset: 0, source_offset: 3 * 1024, length: 2 * 1024 },
                Extent { mft_offset: 2 * 1024, source_offset: 0, length: 3 * 1024 },
            ],
        };
        let mut seen = Vec::new();
        let stats = scan(&Memory(source), &layout, |records| {
            seen.extend(records.into_iter().map(|r| (r.number, r.names[0].name.clone())));
        })
        .unwrap();
        assert_eq!(seen, [(0, "zero".into()), (1, "one".into()), (2, "two".into())]);
        assert_eq!((stats.records, stats.parsed, stats.torn), (5, 3, 1));
    }
}
