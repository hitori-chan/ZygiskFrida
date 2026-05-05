use std::ffi::{c_int, c_void};
use std::fs;
use std::ptr;

use crate::paths::{basename, last_errno};

const MREMAP_MAYMOVE: c_int = 1;
const MREMAP_FIXED: c_int = 2;

unsafe extern "C" {
    fn mremap(
        old_address: *mut c_void,
        old_size: libc::size_t,
        new_size: libc::size_t,
        flags: c_int,
        new_address: *mut c_void,
    ) -> *mut c_void;
}

pub(crate) fn remap_lib(hints: &[String], label: &str) -> bool {
    let selector = MapSelector::new(hints);
    if selector.is_empty() {
        return false;
    }
    let Ok(maps) = fs::read_to_string("/proc/self/maps") else {
        return false;
    };

    let parsed_maps = parse_proc_maps(&maps);
    let mut modules: Vec<_> = parsed_maps
        .iter()
        .filter(|mapping| selector.matches_exact(mapping))
        .cloned()
        .collect();
    if modules.is_empty() {
        modules = parsed_maps
            .into_iter()
            .filter(|mapping| selector.matches_basename(mapping))
            .collect();
    }
    if modules.is_empty() {
        return false;
    }

    if crate::logging::verbose_diagnostics() {
        logi!("Remapping {label} with hints {:?}", selector.display_hints);
    } else {
        logi!("Remapping {label}");
    }
    let mut failed = 0;
    for info in &modules {
        if !remap_mapping(info, label) {
            failed += 1;
        }
    }
    if failed == 0 {
        logi!("Remapped {} mappings", modules.len());
        true
    } else {
        loge!(
            "Remap partially failed: {failed}/{} mappings failed",
            modules.len()
        );
        false
    }
}

fn remap_mapping(info: &ProcMapInfo, label: &str) -> bool {
    let size = info.end.saturating_sub(info.start);
    if size == 0 {
        if crate::logging::verbose_diagnostics() {
            loge!(
                "Skipping zero-length mapping for {label} at {:x}",
                info.start
            );
        } else {
            loge!("Skipping zero-length mapping for {label}");
        }
        return false;
    }
    if info.deleted {
        if crate::logging::verbose_diagnostics() {
            loge!(
                "Skipping deleted mapping for {label}: {}",
                info.path.as_deref().unwrap_or("")
            );
        } else {
            loge!("Skipping deleted mapping for {label}");
        }
        return false;
    }

    let address = info.start as *mut c_void;
    let map = anonymous_writable_map(size);
    if map == libc::MAP_FAILED {
        loge!(
            "Failed to allocate remap memory for {label}: {}",
            last_errno()
        );
        return false;
    }

    let added_read = info.perms & libc::PROT_READ == 0;
    if added_read && protect_mapping(address, size, info.perms | libc::PROT_READ).is_err() {
        loge!(
            "Failed to add read permission before remap for {label}: {}",
            last_errno()
        );
        unmap_mapping(map, size);
        return false;
    }

    copy_mapping(address, map, size);
    if added_read && protect_mapping(address, size, info.perms).is_err() {
        loge!(
            "Failed to restore original protection before mremap for {label}: {}",
            last_errno()
        );
        unmap_mapping(map, size);
        return false;
    }
    let remapped = remap_fixed(map, size, address);
    if remapped == libc::MAP_FAILED {
        loge!("Failed to remap memory for {label}: {}", last_errno());
        unmap_mapping(map, size);
        return false;
    }
    if protect_mapping(address, size, info.perms).is_err() {
        loge!(
            "Failed to restore remapped protection for {label}: {}",
            last_errno()
        );
        return false;
    }

    logi!("Remapped mapping at {address:p} with size {size}");
    true
}

fn anonymous_writable_map(size: usize) -> *mut c_void {
    unsafe {
        libc::mmap(
            ptr::null_mut(),
            size,
            libc::PROT_WRITE,
            libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
            -1,
            0,
        )
    }
}

fn protect_mapping(address: *mut c_void, size: usize, perms: c_int) -> Result<(), ()> {
    if unsafe { libc::mprotect(address, size, perms) } == 0 {
        Ok(())
    } else {
        Err(())
    }
}

fn unmap_mapping(address: *mut c_void, size: usize) {
    unsafe {
        libc::munmap(address, size);
    }
}

fn copy_mapping(src: *mut c_void, dst: *mut c_void, size: usize) {
    unsafe {
        ptr::copy(src.cast::<u8>(), dst.cast::<u8>(), size);
    }
}

fn remap_fixed(map: *mut c_void, size: usize, address: *mut c_void) -> *mut c_void {
    unsafe { mremap(map, size, size, MREMAP_MAYMOVE | MREMAP_FIXED, address) }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcMapInfo {
    start: usize,
    end: usize,
    perms: c_int,
    path: Option<String>,
    deleted: bool,
}

fn parse_proc_maps(maps: &str) -> Vec<ProcMapInfo> {
    maps.lines().filter_map(parse_maps_line).collect()
}

fn parse_maps_line(line: &str) -> Option<ProcMapInfo> {
    let mut parts = line.split_whitespace();
    let range = parts.next()?;
    let perms = parts.next()?;
    let _offset = parts.next()?;
    let _dev = parts.next()?;
    let _inode = parts.next()?;
    let raw_path = parts.collect::<Vec<_>>().join(" ");
    let (path, deleted) = normalize_map_path(&raw_path);
    let (start, end) = range.split_once('-')?;
    let start = usize::from_str_radix(start, 16).ok()?;
    let end = usize::from_str_radix(end, 16).ok()?;

    let mut prot = 0;
    if perms.as_bytes().contains(&b'r') {
        prot |= libc::PROT_READ;
    }
    if perms.as_bytes().contains(&b'w') {
        prot |= libc::PROT_WRITE;
    }
    if perms.as_bytes().contains(&b'x') {
        prot |= libc::PROT_EXEC;
    }

    Some(ProcMapInfo {
        start,
        end,
        perms: prot,
        path,
        deleted,
    })
}

fn normalize_map_path(raw_path: &str) -> (Option<String>, bool) {
    if raw_path.is_empty() {
        return (None, false);
    }
    let Some(path) = raw_path.strip_suffix(" (deleted)") else {
        return (Some(raw_path.to_string()), false);
    };
    (Some(path.to_string()), true)
}

struct MapSelector {
    display_hints: Vec<String>,
    exact_paths: Vec<String>,
    basenames: Vec<String>,
}

impl MapSelector {
    fn new(hints: &[String]) -> Self {
        let mut exact_paths = Vec::new();
        let mut basenames = Vec::new();
        for hint in hints {
            if hint.starts_with('/') {
                exact_paths.push(canonical_or_original(hint));
            }
            if let Some(name) = basename(hint) {
                basenames.push(name.to_string());
            }
        }
        exact_paths.sort();
        exact_paths.dedup();
        basenames.sort();
        basenames.dedup();
        Self {
            display_hints: hints.to_vec(),
            exact_paths,
            basenames,
        }
    }

    fn is_empty(&self) -> bool {
        self.exact_paths.is_empty() && self.basenames.is_empty()
    }

    fn matches_exact(&self, mapping: &ProcMapInfo) -> bool {
        let Some(path) = mapping.path.as_deref() else {
            return false;
        };
        if path.starts_with('[') {
            return false;
        }
        let normalized = canonical_or_original(path);
        self.exact_paths.iter().any(|hint| hint == &normalized)
    }

    fn matches_basename(&self, mapping: &ProcMapInfo) -> bool {
        let Some(path) = mapping.path.as_deref() else {
            return false;
        };
        if path.starts_with('[') {
            return false;
        }
        basename(path).is_some_and(|name| self.basenames.iter().any(|hint| hint == name))
    }
}

fn canonical_or_original(path: &str) -> String {
    fs::canonicalize(path)
        .ok()
        .and_then(|path| path.to_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_maps_line() {
        let line = "7ac49c2000-7ac4a26000 r-xp 00000000 00:00 1245 /x/libgadget.so";
        let parsed = parse_maps_line(line).unwrap();

        assert_eq!(parsed.start, 0x7ac49c2000);
        assert_eq!(parsed.end, 0x7ac4a26000);
        assert_eq!(parsed.perms, libc::PROT_READ | libc::PROT_EXEC);
        assert_eq!(parsed.path.as_deref(), Some("/x/libgadget.so"));
        assert!(!parsed.deleted);
    }

    #[test]
    fn parses_deleted_mapping_explicitly() {
        let line = "7ac49c2000-7ac4a26000 r--p 00000000 00:00 1245 /x/libgadget.so (deleted)";
        let parsed = parse_maps_line(line).unwrap();

        assert_eq!(parsed.path.as_deref(), Some("/x/libgadget.so"));
        assert!(parsed.deleted);
    }

    #[test]
    fn selects_by_exact_path_and_canonical_basename() {
        let selector = MapSelector::new(&["/x/libgadget.so".to_string()]);
        let exact = parse_maps_line("7-8 r-xp 00000000 00:00 1 /x/libgadget.so").unwrap();
        let basename = parse_maps_line("7-8 r-xp 00000000 00:00 1 /other/libgadget.so").unwrap();
        let miss = parse_maps_line("7-8 r-xp 00000000 00:00 1 /x/libgadget2.so").unwrap();

        assert!(selector.matches_exact(&exact));
        assert!(selector.matches_basename(&basename));
        assert!(!selector.matches_exact(&miss));
        assert!(!selector.matches_basename(&miss));
    }

    #[test]
    fn parses_multiple_maps() {
        let maps = "\
7-8 r-xp 00000000 00:00 1 /x/libgadget.so
8-9 rw-p 00001000 00:00 1 /x/libgadget.so
";

        assert_eq!(parse_proc_maps(maps).len(), 2);
    }
}
