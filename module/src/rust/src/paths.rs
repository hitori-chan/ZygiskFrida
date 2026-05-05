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

pub(crate) fn sanitize_component(value: &str) -> Option<String> {
    let mut output = String::with_capacity(value.len().min(180));
    for byte in value.bytes().take(180) {
        let normalized = match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-' => byte as char,
            _ => '_',
        };
        output.push(normalized);
    }

    if output.is_empty() {
        None
    } else {
        Some(output)
    }
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

pub(crate) fn build_stage_names(source_path: &str, index: usize) -> Option<(String, String)> {
    let basename = sanitize_component(basename(source_path)?)?;
    let library_name = format!("{index:04x}-{basename}");
    let config_name = config_name_for_library(&library_name);
    Some((library_name, config_name))
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
        let (library, config) = build_stage_names("libgadget.so", 7).unwrap();

        assert_eq!(library, "0007-libgadget.so");
        assert_eq!(config, "0007-libgadget.config.so");
    }

    #[test]
    fn keeps_libraries_in_subdirs_from_colliding() {
        let (library, config) = build_stage_names("subdir/libgadget-child.so", 0x4000).unwrap();

        assert_eq!(library, "4000-libgadget-child.so");
        assert_eq!(config, "4000-libgadget-child.config.so");
    }

    #[test]
    fn normalizes_unsafe_filename_bytes() {
        let (library, config) = build_stage_names("../lib gadget.so", 1).unwrap();

        assert_eq!(library, "0001-lib_gadget.so");
        assert_eq!(config, "0001-lib_gadget.config.so");
    }

    #[test]
    fn rejects_empty_basename() {
        assert!(build_stage_names("/", 1).is_none());
    }
}
