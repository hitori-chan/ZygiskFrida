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

pub(crate) fn remap_lib(lib_path: &str) -> bool {
    let Some(lib_name) = basename(lib_path) else {
        return false;
    };
    let Ok(maps) = fs::read_to_string("/proc/self/maps") else {
        return false;
    };

    let modules: Vec<_> = maps
        .lines()
        .filter_map(|line| parse_maps_line(line, lib_name))
        .collect();
    if modules.is_empty() {
        return false;
    }

    logi!("Remapping {lib_name}");
    for info in modules {
        if !remap_mapping(&info) {
            return false;
        }
    }
    logi!("Remapped");
    true
}

fn remap_mapping(info: &ProcMapInfo) -> bool {
    let address = info.start as *mut c_void;
    let size = info.end - info.start;
    let map = unsafe {
        libc::mmap(
            ptr::null_mut(),
            size,
            libc::PROT_WRITE,
            libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
            -1,
            0,
        )
    };
    if map == libc::MAP_FAILED {
        loge!("Failed to allocate remap memory: {}", last_errno());
        return false;
    }

    if info.perms & libc::PROT_READ == 0
        && unsafe { libc::mprotect(address, size, info.perms | libc::PROT_READ) } != 0
    {
        loge!("Failed to remove memory protection: {}", last_errno());
        unsafe {
            libc::munmap(map, size);
        }
        return false;
    }

    unsafe {
        ptr::copy(address.cast::<u8>(), map.cast::<u8>(), size);
        let remapped = mremap(map, size, size, MREMAP_MAYMOVE | MREMAP_FIXED, address);
        if remapped == libc::MAP_FAILED {
            loge!("Failed to remap memory: {}", last_errno());
            libc::munmap(map, size);
            return false;
        }
        let _ = libc::mprotect(address, size, info.perms);
    }

    logi!("Allocated at address {map:p} with size of {size}");
    true
}

struct ProcMapInfo {
    start: usize,
    end: usize,
    perms: c_int,
}

fn parse_maps_line(line: &str, lib_name: &str) -> Option<ProcMapInfo> {
    if !line.contains(lib_name) {
        return None;
    }

    let mut parts = line.split_whitespace();
    let range = parts.next()?;
    let perms = parts.next()?;
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_maps_line() {
        let line = "7ac49c2000-7ac4a26000 r-xp 00000000 00:00 1245 /x/libgadget.so";
        let parsed = parse_maps_line(line, "libgadget.so").unwrap();

        assert_eq!(parsed.start, 0x7ac49c2000);
        assert_eq!(parsed.end, 0x7ac4a26000);
        assert_eq!(parsed.perms, libc::PROT_READ | libc::PROT_EXEC);
    }
}
