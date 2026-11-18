//! Fixed-whitelist uncompressed USTAR reader. No tar subprocess, links, PAX or executable hooks.
use super::*;
const MAX_ARCHIVE: u64 = 513 * 1024 * 1024;
fn field(b: &[u8]) -> Result<&str> {
    let end = b.iter().position(|x| *x == 0).unwrap_or(b.len());
    if b[end..].iter().any(|x| *x != 0) {
        return reject("invalid_archive_header");
    }
    std::str::from_utf8(&b[..end]).map_err(|_| Failure("invalid_archive_header"))
}
fn octal(b: &[u8]) -> Result<u64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Failure("invalid_archive_header"))?
        .trim_matches(['\0', ' ']);
    if s.is_empty() || !s.bytes().all(|x| (b'0'..=b'7').contains(&x)) {
        return reject("invalid_archive_number");
    }
    u64::from_str_radix(s, 8).map_err(|_| Failure("invalid_archive_number"))
}
pub(super) fn unpack(path: &Path, destination: &Path) -> Result<Value> {
    absolute(destination)?;
    let mut source = open_private(path, false, false)?;
    if source.metadata()?.len() > MAX_ARCHIVE {
        return reject("archive_size_limit");
    }
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let created = fs::symlink_metadata(destination)?;
    let result = (|| {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(destination.join("bin"))?;
        let allowed: BTreeSet<String> = PAYLOAD
            .iter()
            .map(|p| p.to_string())
            .chain(["manifest.json".into()])
            .collect();
        let mut seen = BTreeSet::new();
        let mut directory_seen = false;
        let mut total = 0u64;
        loop {
            let mut header = [0u8; 512];
            source.read_exact(&mut header)?;
            if header.iter().all(|x| *x == 0) {
                source.read_exact(&mut header)?;
                if header.iter().any(|x| *x != 0) {
                    return reject("archive_trailer_invalid");
                }
                let mut rest = vec![];
                source.take(1024 * 1024 + 1).read_to_end(&mut rest)?;
                if rest.len() > 1024 * 1024 || rest.iter().any(|x| *x != 0) {
                    return reject("archive_trailer_invalid");
                }
                break;
            }
            if &header[257..263] != b"ustar\0"
                || &header[263..265] != b"00"
                || header[345..500].iter().any(|x| *x != 0)
            {
                return reject("archive_format_unsupported");
            }
            let checksum = octal(&header[148..156])?;
            let actual: u64 = header
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    if (148..156).contains(&i) {
                        32
                    } else {
                        *b as u64
                    }
                })
                .sum();
            if checksum != actual {
                return reject("archive_header_checksum");
            }
            let name = field(&header[..100])?;
            if !field(&header[157..257])?.is_empty() {
                return reject("archive_links_forbidden");
            }
            let size = octal(&header[124..136])?;
            if header[156] == b'5' && name == "bin/" && size == 0 && !directory_seen {
                directory_seen = true;
                continue;
            }
            if !matches!(header[156], 0 | b'0')
                || !allowed.contains(name)
                || !seen.insert(name.to_string())
            {
                return reject("archive_payload_forbidden");
            }
            total = total
                .checked_add(size)
                .ok_or(Failure("archive_size_limit"))?;
            if total > 512 * 1024 * 1024
                || size == 0
                || (name == "manifest.json" && size > LIMIT as u64)
            {
                return reject("archive_size_limit");
            }
            let mode = if name.starts_with("bin/") || name.ends_with(".command") {
                0o700
            } else {
                0o600
            };
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .custom_flags(libc::O_NOFOLLOW)
                .open(destination.join(name))?;
            let mut remaining = size;
            let mut buffer = [0u8; 32768];
            while remaining > 0 {
                let n = (remaining as usize).min(buffer.len());
                source.read_exact(&mut buffer[..n])?;
                output.write_all(&buffer[..n])?;
                remaining -= n as u64;
            }
            output.sync_all()?;
            let padding = ((512 - size % 512) % 512) as usize;
            let mut pad = [0u8; 512];
            source.read_exact(&mut pad[..padding])?;
            if pad[..padding].iter().any(|x| *x != 0) {
                return reject("archive_padding_invalid");
            }
        }
        if !seen.contains("manifest.json") {
            return reject("archive_payload_incomplete");
        }
        let manifest: Manifest = read_json(&destination.join("manifest.json"))?;
        let expected: BTreeSet<String> = bundle_payload(&manifest.schema_version)?
            .iter()
            .map(|p| p.to_string())
            .chain(["manifest.json".into()])
            .collect();
        if seen != expected {
            return reject("archive_payload_incomplete");
        }
        let m = bundle_info(destination)?;
        File::open(destination.join("bin"))?.sync_all()?;
        File::open(destination)?.sync_all()?;
        Ok(
            json!({"status":"unpacked_and_checked","directory":destination,"bundle_id":m.bundle_id,"build_profile":m.build_profile,"signature_verified":false,"executed":false,"downloaded":false}),
        )
    })();
    if result.is_err()
        && fs::symlink_metadata(destination)
            .is_ok_and(|m| m.is_dir() && m.ino() == created.ino() && m.dev() == created.dev())
    {
        let _ = fs::remove_dir_all(destination);
    }
    result
}
