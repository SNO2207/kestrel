//! Parsing the NTFS master file table (MFT): one fixed-size record per file, holding its names
//! (with parent folders), timestamps, attributes and sizes. Reading every record straight
//! from the volume is how the whole drive is indexed in seconds instead of crawled folder by
//! folder.
//!
//! The parsing here is pure and works on bytes; [`crate::volume`] supplies them.

/// Records below this number are the file system's own metadata files ($MFT, $LogFile, …),
/// except for the root folder, which is record 5.
pub const FIRST_USER_RECORD: u64 = 16;
pub const ROOT_RECORD: u64 = 5;

/// Multi-sector protection works in units of 512 bytes whatever the sector size.
const FIXUP_STRIDE: usize = 512;
const IN_USE: u16 = 0x1;
const DIRECTORY: u16 = 0x2;

const STANDARD_INFORMATION: u32 = 0x10;
const ATTRIBUTE_LIST: u32 = 0x20;
const FILE_NAME: u32 = 0x30;
const DATA: u32 = 0x80;
const REPARSE_POINT: u32 = 0xC0;
const END: u32 = 0xFFFF_FFFF;

const ATTR_COMPRESSED: u16 = 0x0001;
const ATTR_SPARSE: u16 = 0x8000;
/// $FILE_NAME namespace holding only the 8.3 short name, which duplicates another name.
const DOS_NAMESPACE: u8 = 2;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// The record number part of a file reference; the top 16 bits are its sequence number.
pub fn record_number(reference: u64) -> u64 {
    reference & 0x0000_FFFF_FFFF_FFFF
}

pub fn sequence_number(reference: u64) -> u16 {
    (reference >> 48) as u16
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Name {
    /// File reference of the containing folder.
    pub parent: u64,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DataSize {
    pub size: u64,
    /// Bytes actually allocated: zero for data stored inside the record itself, and the
    /// compressed size for compressed or sparse files.
    pub alloc: u64,
}

/// What one MFT record says about a file. A file with many names or a very fragmented data
/// stream spills into extension records, whose fields are merged into the base record's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub number: u64,
    pub sequence: u16,
    /// For an extension record, the record number of the base record it belongs to.
    pub base: Option<u64>,
    pub is_dir: bool,
    /// Last write time as a FILETIME, from $STANDARD_INFORMATION.
    pub modified: Option<i64>,
    /// Windows file attributes, with the directory bit filled in for folders.
    pub attrs: Option<u32>,
    /// Hard links each have a name; the 8.3 short names are left out.
    pub names: Vec<Name>,
    /// Size of the unnamed data stream, when this record holds its first part.
    pub data: Option<DataSize>,
    pub reparse_tag: Option<u32>,
    /// Data stream runs, kept only when asked for (to locate the MFT itself).
    pub runs: Option<(u64, Vec<Run>)>,
    /// $ATTRIBUTE_LIST entries for data streams held in other records.
    pub data_elsewhere: Vec<u64>,
}

/// A contiguous stretch of a non-resident stream: `clusters` clusters starting at cluster
/// `lcn` on the volume, or a hole when `lcn` is `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub lcn: Option<i64>,
    pub clusters: u64,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// Undoes multi-sector protection: the last two bytes of every 512-byte stride were replaced
/// on disk by a check value, with the originals kept in the update sequence array. Returns
/// false for a torn or corrupt record, whose strides do not all carry the check value.
pub fn apply_fixups(record: &mut [u8]) -> bool {
    let (Some(offset), Some(count)) = (u16_at(record, 4), u16_at(record, 6)) else { return false };
    let (offset, count) = (offset as usize, count as usize);
    if count < 2 || offset + count * 2 > record.len() || (count - 1) * FIXUP_STRIDE > record.len() {
        return false;
    }
    let check = [record[offset], record[offset + 1]];
    for i in 1..count {
        let end = i * FIXUP_STRIDE;
        if record[end - 2..end] != check {
            return false;
        }
        let original = [record[offset + 2 * i], record[offset + 2 * i + 1]];
        record[end - 2..end].copy_from_slice(&original);
    }
    true
}

/// Decodes a data run list into runs of absolute cluster numbers.
pub fn decode_runs(mut bytes: &[u8]) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut lcn: i64 = 0;
    loop {
        let &header = bytes.first()?;
        if header == 0 {
            return Some(runs);
        }
        let (length_size, offset_size) = ((header & 0x0F) as usize, (header >> 4) as usize);
        if length_size == 0 || length_size > 8 || offset_size > 8 {
            return None;
        }
        let fields = bytes.get(1..1 + length_size + offset_size)?;
        let mut clusters = 0u64;
        for (i, &byte) in fields[..length_size].iter().enumerate() {
            clusters |= (byte as u64) << (8 * i);
        }
        let run_lcn = if offset_size == 0 {
            None
        } else {
            let mut delta = 0i64;
            for (i, &byte) in fields[length_size..].iter().enumerate() {
                delta |= (byte as i64) << (8 * i);
            }
            // Offsets are signed and relative to the previous run.
            let unused = 64 - 8 * offset_size as u32;
            delta = (delta << unused) >> unused;
            lcn += delta;
            Some(lcn)
        };
        runs.push(Run { lcn: run_lcn, clusters });
        bytes = &bytes[1 + length_size + offset_size..];
    }
}

/// Parses one record whose fixups have already been applied. Returns `None` for records that
/// are unused or malformed. With `keep_runs`, the unnamed data stream's runs are kept too.
pub fn parse_record(record: &[u8], number: u64, keep_runs: bool) -> Option<Record> {
    if record.get(..4)? != b"FILE" {
        return None;
    }
    let flags = u16_at(record, 22)?;
    if flags & IN_USE == 0 {
        return None;
    }
    let used = (u32_at(record, 24)? as usize).min(record.len());
    let base = record_number(u64_at(record, 32)?);
    let is_dir = flags & DIRECTORY != 0;
    let mut out = Record {
        number,
        sequence: u16_at(record, 16)?,
        base: (base != 0).then_some(base),
        is_dir,
        ..Record::default()
    };

    let mut at = u16_at(record, 20)? as usize;
    while at + 16 <= used {
        let kind = u32_at(record, at)?;
        if kind == END {
            break;
        }
        let length = u32_at(record, at + 4)? as usize;
        if length < 16 || at + length > used {
            break;
        }
        let attribute = &record[at..at + length];
        let resident = attribute[8] == 0;
        let named = attribute[9] != 0;
        let value = || -> Option<&[u8]> {
            let size = u32_at(attribute, 16)? as usize;
            let offset = u16_at(attribute, 20)? as usize;
            attribute.get(offset..offset + size)
        };

        match kind {
            STANDARD_INFORMATION if resident => {
                if let Some(value) = value() {
                    out.modified = u64_at(value, 8).map(|t| t as i64);
                    out.attrs = u32_at(value, 32).map(|a| if is_dir { a | FILE_ATTRIBUTE_DIRECTORY } else { a });
                }
            }
            FILE_NAME if resident => {
                if let Some(value) = value() {
                    let length = *value.get(64)? as usize;
                    if value.get(65) != Some(&DOS_NAMESPACE) {
                        let units: Vec<u16> = value
                            .get(66..66 + 2 * length)?
                            .chunks_exact(2)
                            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                            .collect();
                        out.names.push(Name { parent: u64_at(value, 0)?, name: String::from_utf16_lossy(&units) });
                    }
                }
            }
            DATA if !named => {
                if resident {
                    let size = u32_at(attribute, 16)? as u64;
                    out.data = Some(DataSize { size, alloc: 0 });
                } else {
                    let first_vcn = u64_at(attribute, 16)?;
                    // Only the stretch that starts the stream carries its sizes.
                    if first_vcn == 0 {
                        let flags = u16_at(attribute, 12)?;
                        let compressed = flags & (ATTR_COMPRESSED | ATTR_SPARSE) != 0;
                        let alloc = if compressed && length >= 72 { u64_at(attribute, 64)? } else { u64_at(attribute, 40)? };
                        out.data = Some(DataSize { size: u64_at(attribute, 48)?, alloc });
                    }
                    if keep_runs {
                        let runs_at = u16_at(attribute, 32)? as usize;
                        out.runs = Some((first_vcn, decode_runs(attribute.get(runs_at..)?)?));
                    }
                }
            }
            REPARSE_POINT if resident => {
                out.reparse_tag = value().and_then(|v| u32_at(v, 0));
            }
            ATTRIBUTE_LIST if resident && keep_runs => {
                let mut list = value()?;
                while list.len() >= 26 {
                    let entry_length = u16_at(list, 4)? as usize;
                    if entry_length < 26 || entry_length > list.len() {
                        break;
                    }
                    let elsewhere = record_number(u64_at(list, 16)?);
                    if u32_at(list, 0)? == DATA && list[6] == 0 && elsewhere != number {
                        out.data_elsewhere.push(elsewhere);
                    }
                    list = &list[entry_length..];
                }
            }
            _ => {}
        }
        at += length;
    }
    Some(out)
}

/// Building MFT records byte by byte, for tests here and in crates that consume them.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::*;

    /// Builds MFT records attribute by attribute, applying multi-sector protection at the end
    /// exactly as NTFS writes them to disk.
    pub struct RecordBuilder {
        pub bytes: Vec<u8>,
        at: usize,
    }

    impl RecordBuilder {
        pub fn new(size: usize, flags: u16, sequence: u16, base: u64) -> Self {
            let mut bytes = vec![0u8; size];
            bytes[..4].copy_from_slice(b"FILE");
            let strides = size / FIXUP_STRIDE;
            bytes[4..6].copy_from_slice(&48u16.to_le_bytes());
            bytes[6..8].copy_from_slice(&((strides + 1) as u16).to_le_bytes());
            bytes[16..18].copy_from_slice(&sequence.to_le_bytes());
            bytes[22..24].copy_from_slice(&flags.to_le_bytes());
            bytes[28..32].copy_from_slice(&(size as u32).to_le_bytes());
            bytes[32..40].copy_from_slice(&base.to_le_bytes());
            let first = (48 + 2 * (strides + 1)).next_multiple_of(8);
            bytes[20..22].copy_from_slice(&(first as u16).to_le_bytes());
            Self { bytes, at: first }
        }

        fn attribute(&mut self, kind: u32, header: &[u8], body: &[u8]) -> &mut Self {
            let length = (header.len() + body.len()).next_multiple_of(8);
            let at = self.at;
            self.bytes[at..at + 4].copy_from_slice(&kind.to_le_bytes());
            self.bytes[at + 4..at + 8].copy_from_slice(&(length as u32).to_le_bytes());
            self.bytes[at + 8..at + header.len()].copy_from_slice(&header[8..]);
            self.bytes[at + header.len()..at + header.len() + body.len()].copy_from_slice(body);
            self.at += length;
            self
        }

        pub fn resident(&mut self, kind: u32, named: bool, value: &[u8]) -> &mut Self {
            let mut header = vec![0u8; 24];
            header[9] = named as u8;
            header[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes());
            header[20..22].copy_from_slice(&24u16.to_le_bytes());
            self.attribute(kind, &header, value)
        }

        pub fn standard_info(&mut self, modified: i64, attrs: u32) -> &mut Self {
            let mut value = vec![0u8; 48];
            value[8..16].copy_from_slice(&modified.to_le_bytes());
            value[32..36].copy_from_slice(&attrs.to_le_bytes());
            self.resident(STANDARD_INFORMATION, false, &value)
        }

        pub fn file_name(&mut self, parent: u64, name: &str, namespace: u8) -> &mut Self {
            let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let mut value = vec![0u8; 66 + units.len()];
            value[..8].copy_from_slice(&parent.to_le_bytes());
            value[64] = (units.len() / 2) as u8;
            value[65] = namespace;
            value[66..].copy_from_slice(&units);
            self.resident(FILE_NAME, false, &value)
        }

        /// A non-resident unnamed data stream.
        pub fn data_runs(&mut self, first_vcn: u64, size: u64, alloc: u64, flags: u16, compressed: Option<u64>, runs: &[u8]) -> &mut Self {
            let header_length = if compressed.is_some() { 72 } else { 64 };
            let mut header = vec![0u8; header_length];
            header[8] = 1;
            header[12..14].copy_from_slice(&flags.to_le_bytes());
            header[16..24].copy_from_slice(&first_vcn.to_le_bytes());
            header[32..34].copy_from_slice(&(header_length as u16).to_le_bytes());
            header[40..48].copy_from_slice(&alloc.to_le_bytes());
            header[48..56].copy_from_slice(&size.to_le_bytes());
            header[56..64].copy_from_slice(&size.to_le_bytes());
            if let Some(compressed) = compressed {
                header[64..72].copy_from_slice(&compressed.to_le_bytes());
            }
            self.attribute(DATA, &header, runs)
        }

        /// Ends the attribute list and protects the record as NTFS does on disk.
        pub fn finish(&mut self) -> Vec<u8> {
            let at = self.at;
            self.bytes[at..at + 4].copy_from_slice(&END.to_le_bytes());
            self.bytes[24..28].copy_from_slice(&((at + 8) as u32).to_le_bytes());
            let mut bytes = self.bytes.clone();
            let check = [0x5Au8, 0xA5];
            bytes[48..50].copy_from_slice(&check);
            for i in 1..=bytes.len() / FIXUP_STRIDE {
                let end = i * FIXUP_STRIDE;
                let original = [bytes[end - 2], bytes[end - 1]];
                bytes[48 + 2 * i..48 + 2 * i + 2].copy_from_slice(&original);
                bytes[end - 2..end].copy_from_slice(&check);
            }
            bytes
        }
    }

}

#[cfg(test)]
mod tests {
    use super::testing::RecordBuilder;
    use super::*;

    const ROOT: u64 = 0x0005_0000_0000_0005;

    #[test]
    fn fixups_restore_the_original_bytes_and_detect_torn_records() {
        let mut builder = RecordBuilder::new(1024, IN_USE, 1, 0);
        builder.standard_info(7, 0x20);
        // Put real data across the first stride boundary.
        builder.bytes[510] = 0x12;
        builder.bytes[511] = 0x34;
        let clean = builder.bytes.clone();
        let mut on_disk = builder.finish();
        assert_eq!(&on_disk[510..512], &[0x5A, 0xA5]);
        assert!(apply_fixups(&mut on_disk));
        assert_eq!(&on_disk[510..512], &clean[510..512]);

        let mut torn = builder.finish();
        torn[1022] ^= 0xFF;
        assert!(!apply_fixups(&mut torn));
    }

    #[test]
    fn parses_names_times_sizes_and_skips_short_names() {
        let mut on_disk = RecordBuilder::new(1024, IN_USE, 3, 0)
            .standard_info(133_000_000_000_000_000, 0x20)
            .file_name(ROOT, "REPORT~1.PDF", DOS_NAMESPACE)
            .file_name(ROOT, "Quarterly report.pdf", 1)
            .data_runs(0, 5_000_000, 5_001_216, 0, None, &[0x21, 0x10, 0x00, 0x01, 0x00])
            .finish();
        assert!(apply_fixups(&mut on_disk));
        let record = parse_record(&on_disk, 1234, true).unwrap();
        assert_eq!(record.sequence, 3);
        assert_eq!(record.modified, Some(133_000_000_000_000_000));
        assert_eq!(record.attrs, Some(0x20));
        assert_eq!(record.names, [Name { parent: ROOT, name: "Quarterly report.pdf".into() }]);
        assert_eq!(record.data, Some(DataSize { size: 5_000_000, alloc: 5_001_216 }));
        assert_eq!(record.runs, Some((0, vec![Run { lcn: Some(256), clusters: 16 }])));
        assert!(!record.is_dir && record.base.is_none());
    }

    #[test]
    fn folders_hard_links_resident_and_compressed_data() {
        let mut folder = RecordBuilder::new(1024, IN_USE | DIRECTORY, 1, 0)
            .standard_info(1, 0x10_0000)
            .file_name(ROOT, "Projects", 3)
            .finish();
        assert!(apply_fixups(&mut folder));
        let folder = parse_record(&folder, 40, false).unwrap();
        assert!(folder.is_dir);
        assert_eq!(folder.attrs, Some(0x10_0000 | FILE_ATTRIBUTE_DIRECTORY), "the directory bit is added");

        let mut linked = RecordBuilder::new(1024, IN_USE, 1, 0)
            .standard_info(1, 0x20)
            .file_name(ROOT, "a.txt", 1)
            .file_name(0x0001_0000_0000_0028, "b.txt", 1)
            .resident(DATA, false, b"hello")
            .resident(DATA, true, b"an alternate stream, not the file's size")
            .finish();
        assert!(apply_fixups(&mut linked));
        let linked = parse_record(&linked, 41, false).unwrap();
        assert_eq!(linked.names.len(), 2);
        assert_eq!(linked.data, Some(DataSize { size: 5, alloc: 0 }));

        let mut compressed = RecordBuilder::new(1024, IN_USE, 1, 0)
            .data_runs(0, 1_000_000, 1_048_576, ATTR_COMPRESSED, Some(65_536), &[0x11, 0x10, 0x20, 0x00])
            .finish();
        assert!(apply_fixups(&mut compressed));
        assert_eq!(parse_record(&compressed, 42, false).unwrap().data, Some(DataSize { size: 1_000_000, alloc: 65_536 }));
    }

    #[test]
    fn extension_records_and_unused_records() {
        let mut extension = RecordBuilder::new(1024, IN_USE, 1, 0x0002_0000_0000_0063)
            .data_runs(0, 9, 4096, 0, None, &[0x11, 0x01, 0x05, 0x00])
            .finish();
        assert!(apply_fixups(&mut extension));
        let extension = parse_record(&extension, 500, false).unwrap();
        assert_eq!(extension.base, Some(0x63));

        // A later stretch of a fragmented stream says nothing about the size.
        let mut later = RecordBuilder::new(1024, IN_USE, 1, 0x63).data_runs(64, 9, 4096, 0, None, &[0x11, 0x01, 0x05, 0x00]).finish();
        assert!(apply_fixups(&mut later));
        assert_eq!(parse_record(&later, 501, false).unwrap().data, None);

        let mut unused = RecordBuilder::new(1024, 0, 1, 0).finish();
        assert!(apply_fixups(&mut unused));
        assert!(parse_record(&unused, 502, false).is_none());
        assert!(parse_record(b"BAAD", 503, false).is_none());
    }

    #[test]
    fn decodes_runs_with_negative_offsets_and_holes() {
        // 0x40 clusters at 0x1000, then 0x10 clusters 0x800 back, then a 4-cluster hole.
        let runs = decode_runs(&[0x21, 0x40, 0x00, 0x10, 0x21, 0x10, 0x00, 0xF8, 0x01, 0x04, 0x00]).unwrap();
        assert_eq!(runs, [
            Run { lcn: Some(0x1000), clusters: 0x40 },
            Run { lcn: Some(0x800), clusters: 0x10 },
            Run { lcn: None, clusters: 4 },
        ]);
        assert!(decode_runs(&[0x21, 0x40]).is_none(), "truncated");
    }

    #[test]
    fn references_split_into_record_and_sequence() {
        assert_eq!(record_number(0x0005_0000_0000_0005), 5);
        assert_eq!(sequence_number(0x0005_0000_0000_0005), 5);
    }
}
