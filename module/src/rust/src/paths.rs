use std::ffi::CString;
use std::fs;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use libc::{c_int, c_uint, mode_t};

pub(crate) fn c_string(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| format!("string contains NUL byte: {value:?}"))
}

pub(crate) fn last_errno() -> String {
    std::io::Error::last_os_error().to_string()
}

pub(crate) fn close_fd(fd: c_int) {
    if fd >= 0 {
        unsafe {
            libc::close(fd);
        }
    }
}

pub(crate) fn current_pid() -> libc::pid_t {
    unsafe { libc::getpid() }
}

#[cfg(test)]
pub(crate) fn current_uid() -> libc::uid_t {
    unsafe { libc::getuid() }
}

#[cfg(test)]
pub(crate) fn current_gid() -> libc::gid_t {
    unsafe { libc::getgid() }
}

pub(crate) fn duplicate_fd(fd: c_int) -> Result<OwnedFd, String> {
    let dup_fd = unsafe { libc::dup(fd) };
    if dup_fd < 0 {
        return Err(last_errno());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(dup_fd) })
}

pub(crate) fn module_fd_path(module_dir_fd: c_int, name: &str) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{module_dir_fd}/{name}"))
}

pub(crate) fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/')
}

pub(crate) fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(index) => &path[..index],
        None => ".",
    }
}

pub(crate) fn basename(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(name)
}

pub(crate) fn config_name_for_library(library_name: &str) -> String {
    if let Some(stem) = library_name.strip_suffix(".so") {
        format!("{stem}.config.so")
    } else {
        format!("{library_name}.config")
    }
}

pub(crate) fn sidecar_name_for_library(library_path: &str) -> Option<String> {
    Some(config_name_for_library(basename(library_path)?))
}

pub(crate) fn build_stage_names(
    source_path: &str,
    role: &str,
    index: usize,
) -> Option<(String, String)> {
    basename(source_path)?;
    let hash = fnv1a64_stage_hash(source_path, role, index);
    let stem = format!("{index:04x}-{hash:016x}");
    let library_name = format!("{stem}.so");
    let config_name = format!("{stem}.config.so");
    Some((library_name, config_name))
}

fn fnv1a64_stage_hash(source_path: &str, role: &str, index: usize) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET;
    let index_bytes = index.to_le_bytes();
    for bytes in [
        source_path.as_bytes(),
        b"\0",
        role.as_bytes(),
        b"\0",
        &index_bytes,
    ] {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

pub(crate) fn openat_owned(
    dir_fd: c_int,
    path: &str,
    flags: c_int,
    mode: mode_t,
) -> Result<OwnedFd, String> {
    let path = c_string(path)?;
    let fd = unsafe { libc::openat(dir_fd, path.as_ptr(), flags, mode as c_uint) };
    if fd < 0 {
        return Err(last_errno());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub(crate) fn open_owned(path: &Path, flags: c_int, mode: mode_t) -> Result<OwnedFd, String> {
    let path = path
        .to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))?;
    let path = c_string(path)?;
    let fd = unsafe { libc::open(path.as_ptr(), flags, mode as c_uint) };
    if fd < 0 {
        return Err(last_errno());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub(crate) fn read_text_file(path: &Path) -> std::io::Result<String> {
    fs::read_to_string(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_config_name_for_gadget_so() {
        let (library, config) = build_stage_names("libgadget.so", "parent", 7).unwrap();

        assert!(library.starts_with("0007-"));
        assert!(library.ends_with(".so"));
        assert_eq!(config, library.replace(".so", ".config.so"));
        assert!(!library.contains("libgadget"));
    }

    #[test]
    fn keeps_libraries_in_subdirs_from_colliding() {
        let (library, config) =
            build_stage_names("subdir/libgadget-child.so", "child", 0x4000).unwrap();

        assert!(library.starts_with("4000-"));
        assert!(library.ends_with(".so"));
        assert_eq!(config, library.replace(".so", ".config.so"));
        assert!(!library.contains("libgadget"));
    }

    #[test]
    fn generated_stage_names_are_deterministic() {
        let first = build_stage_names("../lib gadget.so", "parent", 1).unwrap();
        let second = build_stage_names("../lib gadget.so", "parent", 1).unwrap();
        let child = build_stage_names("../lib gadget.so", "child", 1).unwrap();

        assert_eq!(first, second);
        assert_ne!(first, child);
        assert_eq!(first.0.len(), "0001-0000000000000000.so".len());
    }

    #[test]
    fn rejects_empty_basename() {
        assert!(build_stage_names("/", "parent", 1).is_none());
    }
}
