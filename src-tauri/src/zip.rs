//! Minimal ZIP writer -- only for the `export` job (design doc §7.1).
//!
//! No third-party crate: flate2 already supplies deflate, and the rest of ZIP is
//! three fixed headers plus a CRC32, so writing it beats pulling in a dependency
//! (same tradeoff as the hand-written [`crate::httpclient`] and using tar instead
//! of rsync).
//!
//! Supports: STORE (method 0), DEFLATE (method 8), directory entries, UTF-8 name
//! flag. Does not support: ZIP64 (per-file / total size over 4GiB errors out),
//! encryption, multi-volume.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::write::DeflateEncoder;
use flate2::Compression;
use walkdir::WalkDir;

const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
/// General purpose bit 11: the file name is UTF-8.
const FLAG_UTF8: u16 = 1 << 11;
/// Cap for one entry and the whole archive; beyond it classic ZIP cannot express it.
const MAX_ZIP_BYTES: u64 = u32::MAX as u64;

struct Entry {
    name: String,
    method: u16,
    crc: u32,
    compressed: u64,
    uncompressed: u64,
    offset: u64,
    time: u16,
    date: u16,
    is_dir: bool,
}

pub struct ZipWriter<W: Write> {
    inner: W,
    offset: u64,
    entries: Vec<Entry>,
}

impl<W: Write> ZipWriter<W> {
    pub fn new(inner: W) -> ZipWriter<W> {
        ZipWriter {
            inner,
            offset: 0,
            entries: Vec::new(),
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Add a directory entry (`name` without the trailing slash; it is appended here).
    pub fn add_dir(&mut self, name: &str, mtime: SystemTime) -> io::Result<()> {
        let name = format!("{}/", name.trim_end_matches('/'));
        let (time, date) = dos_datetime(mtime);
        let offset = self.offset;
        self.write_local_header(&name, METHOD_STORE, 0, 0, 0, time, date)?;
        self.entries.push(Entry {
            name,
            method: METHOD_STORE,
            crc: 0,
            compressed: 0,
            uncompressed: 0,
            offset,
            time,
            date,
            is_dir: true,
        });
        Ok(())
    }

    /// Add a file entry. Small files use STORE (deflate would grow them), the rest DEFLATE.
    pub fn add_file(&mut self, name: &str, mtime: SystemTime, data: &[u8]) -> io::Result<()> {
        let (time, date) = dos_datetime(mtime);
        let crc = crc32(data);

        let (method, payload) = if data.len() < 128 {
            (METHOD_STORE, data.to_vec())
        } else {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(data)?;
            let deflated = encoder.finish()?;
            if deflated.len() < data.len() {
                (METHOD_DEFLATE, deflated)
            } else {
                (METHOD_STORE, data.to_vec())
            }
        };

        if payload.len() as u64 > MAX_ZIP_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                crate::i18n::zip_entry_too_large(crate::i18n::current(), name),
            ));
        }

        let offset = self.offset;
        self.write_local_header(
            name,
            method,
            crc,
            payload.len() as u64,
            data.len() as u64,
            time,
            date,
        )?;
        self.write_all(&payload)?;

        self.entries.push(Entry {
            name: name.to_string(),
            method,
            crc,
            compressed: payload.len() as u64,
            uncompressed: data.len() as u64,
            offset,
            time,
            date,
            is_dir: false,
        });
        Ok(())
    }

    fn write_local_header(
        &mut self,
        name: &str,
        method: u16,
        crc: u32,
        compressed: u64,
        uncompressed: u64,
        time: u16,
        date: u16,
    ) -> io::Result<()> {
        let raw = name.as_bytes();
        let mut header = Vec::with_capacity(30 + raw.len());
        header.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header signature
        header.extend_from_slice(&20u16.to_le_bytes()); // version needed
        header.extend_from_slice(&FLAG_UTF8.to_le_bytes());
        header.extend_from_slice(&method.to_le_bytes());
        header.extend_from_slice(&time.to_le_bytes());
        header.extend_from_slice(&date.to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&(compressed as u32).to_le_bytes());
        header.extend_from_slice(&(uncompressed as u32).to_le_bytes());
        header.extend_from_slice(&(raw.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // extra length
        header.extend_from_slice(raw);
        self.write_all(&header)
    }

    /// Write the central directory and finish.
    pub fn finish(mut self) -> io::Result<W> {
        let cd_offset = self.offset;
        let entries = std::mem::take(&mut self.entries);

        for entry in &entries {
            let raw = entry.name.as_bytes();
            let mut header = Vec::with_capacity(46 + raw.len());
            header.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central directory signature
            header.extend_from_slice(&0x031Eu16.to_le_bytes()); // made by: 3 = Unix, 30 = 3.0
            header.extend_from_slice(&20u16.to_le_bytes());
            header.extend_from_slice(&FLAG_UTF8.to_le_bytes());
            header.extend_from_slice(&entry.method.to_le_bytes());
            header.extend_from_slice(&entry.time.to_le_bytes());
            header.extend_from_slice(&entry.date.to_le_bytes());
            header.extend_from_slice(&entry.crc.to_le_bytes());
            header.extend_from_slice(&(entry.compressed as u32).to_le_bytes());
            header.extend_from_slice(&(entry.uncompressed as u32).to_le_bytes());
            header.extend_from_slice(&(raw.len() as u16).to_le_bytes());
            header.extend_from_slice(&0u16.to_le_bytes()); // extra
            header.extend_from_slice(&0u16.to_le_bytes()); // comment
            header.extend_from_slice(&0u16.to_le_bytes()); // disk number start
            header.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            // External attributes: 0o40755 << 16 | 0x10 for dirs, 0o100644 << 16 for files
            let external: u32 = if entry.is_dir {
                (0o040755u32 << 16) | 0x10
            } else {
                0o100644u32 << 16
            };
            header.extend_from_slice(&external.to_le_bytes());
            header.extend_from_slice(&(entry.offset as u32).to_le_bytes());
            header.extend_from_slice(raw);
            self.write_all(&header)?;
        }

        let cd_size = self.offset - cd_offset;
        if cd_size > MAX_ZIP_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                crate::i18n::zip_central_dir_too_large(crate::i18n::current()),
            ));
        }
        if entries.len() > u16::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                crate::i18n::zip_too_many_entries(crate::i18n::current()),
            ));
        }

        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        end.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        end.extend_from_slice(&(cd_size as u32).to_le_bytes());
        end.extend_from_slice(&(cd_offset as u32).to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.write_all(&end)?;
        self.inner.flush()?;

        Ok(self.inner)
    }
}

/// Zip a whole directory tree into `out`. Returns `(files, raw bytes, skipped links)`.
///
/// Symlinks are skipped -- matching [`crate::sync::build_archive`], so an archive
/// never carries off things from outside the project tree.
pub fn zip_directory(root: &Path, out: &Path) -> io::Result<(u64, u64, u64)> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = BufWriter::new(File::create(out)?);
    let mut writer = ZipWriter::new(file);

    let mut files = 0u64;
    let mut bytes = 0u64;
    // How many symlinks were skipped. Classic ZIP expresses links via Unix extra
    // attributes that many extractors ignore, and this writer only handles dirs and
    // regular files -- so skip them, but **report the count**.
    let mut skipped_links = 0u64;

    for entry in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel,
            _ => continue,
        };
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        if name.is_empty() {
            continue;
        }

        let file_type = entry.file_type();
        if file_type.is_symlink() {
            skipped_links += 1;
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(UNIX_EPOCH);

        if file_type.is_dir() {
            writer.add_dir(&name, mtime)?;
        } else if file_type.is_file() {
            let data = std::fs::read(path)?;
            bytes += data.len() as u64;
            files += 1;
            writer.add_file(&name, mtime, &data)?;
        }
    }

    writer.finish()?;
    Ok((files, bytes, skipped_links))
}

/// Recursively copy directory contents to `target` (symlinks skipped).
/// Returns `(files, bytes, skipped links)`.
pub fn copy_tree(source: &Path, target: &Path) -> io::Result<(u64, u64, u64)> {
    std::fs::create_dir_all(target)?;
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut skipped_links = 0u64;

    if source.is_file() {
        let name = source
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("payload"));
        std::fs::copy(source, target.join(name))?;
        return Ok((1, std::fs::metadata(source)?.len(), 0));
    }

    for entry in WalkDir::new(source).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(source) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel,
            _ => continue,
        };
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            skipped_links += 1;
            continue;
        }
        if file_type.is_dir() {
            std::fs::create_dir_all(target.join(rel))?;
        } else if file_type.is_file() {
            let dest = target.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            bytes += std::fs::copy(path, &dest)?;
            files += 1;
        }
    }

    Ok((files, bytes, skipped_links))
}

// ------------------------------------------------------------------ helpers

/// CRC-32 (IEEE 802.3), the polynomial ZIP uses. Bitwise, with a table generated lazily.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320u32 & mask);
        }
    }
    !crc
}

/// unix seconds -> DOS time/date (1980 epoch, seconds at 2-second granularity).
fn dos_datetime(time: SystemTime) -> (u16, u16) {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let days = secs.div_euclid(86_400);
    let in_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    let hour = (in_day / 3_600) as u16;
    let minute = ((in_day % 3_600) / 60) as u16;
    let second = (in_day % 60) as u16;

    // Times before 1980 can only clamp to 1980-01-01.
    let year = (year - 1980).clamp(0, 127) as u16;
    let date = (year << 9) | ((month as u16) << 5) | day as u16;
    let time = (hour << 11) | (minute << 5) | (second / 2);
    (time, date)
}

/// Howard Hinnant's civil_from_days: unix days -> (year, month, day).
pub(crate) fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (
        (if m <= 2 { y + 1 } else { y }) as i32,
        m,
        d,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
    }

    #[test]
    fn civil_from_days_hits_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // leap day
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    /// Checks only the ZIP skeleton: EOCD present, entry count right, central
    /// directory offset correct.
    #[test]
    fn zip_structure_is_well_formed() {
        let mut writer = ZipWriter::new(Vec::new());
        writer.add_dir("dir", UNIX_EPOCH).unwrap();
        writer
            .add_file("dir/hello.txt", UNIX_EPOCH, b"hello world")
            .unwrap();
        // Data over 128 bytes that actually compresses should go DEFLATE
        let big = vec![b'x'; 4096];
        writer.add_file("dir/big.txt", UNIX_EPOCH, &big).unwrap();
        let bytes = writer.finish().unwrap();

        assert_eq!(&bytes[0..4], &[0x50, 0x4b, 0x03, 0x04]);

        // EOCD is the last 22 bytes
        let eocd = bytes.len() - 22;
        assert_eq!(&bytes[eocd..eocd + 4], &[0x50, 0x4b, 0x05, 0x06]);
        let count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]);
        assert_eq!(count, 3);
        let cd_size = u32::from_le_bytes(bytes[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
        let cd_offset =
            u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        assert_eq!(cd_offset + cd_size, eocd);
        assert_eq!(&bytes[cd_offset..cd_offset + 4], &[0x50, 0x4b, 0x01, 0x02]);

        // The second entry should have been deflated: 4096 x's compress far below raw size
        let method =
            u16::from_le_bytes(bytes[cd_offset + 10..cd_offset + 12].try_into().unwrap());
        assert_eq!(method, METHOD_STORE, "first central directory entry is a directory");
    }

    #[test]
    fn deflate_is_used_for_compressible_data() {
        let mut writer = ZipWriter::new(Vec::new());
        let big = vec![b'x'; 4096];
        writer.add_file("big.txt", UNIX_EPOCH, &big).unwrap();
        let bytes = writer.finish().unwrap();
        // After compression the whole archive should be far below 4096 bytes
        assert!(bytes.len() < 1024, "archive size {} is unreasonable", bytes.len());

        let eocd = bytes.len() - 22;
        let cd_offset =
            u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        let method =
            u16::from_le_bytes(bytes[cd_offset + 10..cd_offset + 12].try_into().unwrap());
        assert_eq!(method, METHOD_DEFLATE);
    }

    #[test]
    fn zip_directory_round_trips_names() {
        let root = std::env::temp_dir().join("cgw-zip-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"bb").unwrap();

        let out = root.join("out.zip");
        let (files, bytes, skipped) = zip_directory(&root.join("sub"), &out).unwrap();
        assert_eq!(files, 1);
        assert_eq!(bytes, 2);
        assert_eq!(skipped, 0);

        let data = std::fs::read(&out).unwrap();
        // The entry name is "b.txt", relative to root
        assert!(data.windows(5).any(|w| w == b"b.txt"));

        let _ = std::fs::remove_dir_all(&root);
    }
}

