use std::ffi::{CStr, c_char, c_int, c_void};

pub(crate) enum LoadStatus {
    Opened {
        handle: *mut c_void,
        method: &'static str,
    },
    AlreadyMapped,
}

pub(crate) struct LoadError {
    pub(crate) method: &'static str,
    pub(crate) error: String,
}

#[cfg(target_os = "android")]
const ANDROID_DLEXT_FORCE_LOAD: u64 = 0x40;

#[cfg(target_os = "android")]
#[repr(C)]
#[derive(Default)]
pub(crate) struct AndroidDlExtInfo {
    pub(crate) flags: u64,
    pub(crate) reserved_addr: *mut c_void,
    pub(crate) reserved_size: libc::size_t,
    pub(crate) relro_fd: c_int,
    pub(crate) library_fd: c_int,
    pub(crate) library_fd_offset: i64,
    pub(crate) library_namespace: *mut c_void,
}

#[cfg(target_os = "android")]
#[link(name = "dl")]
unsafe extern "C" {
    fn android_dlopen_ext(
        filename: *const c_char,
        flags: c_int,
        extinfo: *const AndroidDlExtInfo,
    ) -> *mut c_void;
}

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

pub(crate) fn load(path: &CStr, display_path: &str) -> Result<LoadStatus, Vec<LoadError>> {
    if library_is_already_mapped(display_path) {
        return Ok(LoadStatus::AlreadyMapped);
    }

    let mut errors = Vec::new();
    if let Some(result) = try_private_linker_force_load(path, &mut errors) {
        return Ok(result);
    }
    if let Some(result) = try_android_force_load(path, &mut errors) {
        return Ok(result);
    }

    let handle = unsafe { dlopen(path.as_ptr(), libc::RTLD_NOW) };
    if !handle.is_null() {
        return Ok(LoadStatus::Opened {
            handle,
            method: "dlopen",
        });
    }

    errors.push(LoadError {
        method: "dlopen",
        error: dlerror_string(),
    });
    Err(errors)
}

#[cfg(target_os = "android")]
fn try_private_linker_force_load(path: &CStr, errors: &mut Vec<LoadError>) -> Option<LoadStatus> {
    match crate::android_linker::force_dlopen(path) {
        Ok(handle) => Some(LoadStatus::Opened {
            handle,
            method: "private linker force-load",
        }),
        Err(error) => {
            errors.push(LoadError {
                method: "private linker force-load",
                error: error.to_string(),
            });
            None
        }
    }
}

#[cfg(not(target_os = "android"))]
fn try_private_linker_force_load(_path: &CStr, _errors: &mut Vec<LoadError>) -> Option<LoadStatus> {
    None
}

#[cfg(target_os = "android")]
fn try_android_force_load(path: &CStr, errors: &mut Vec<LoadError>) -> Option<LoadStatus> {
    let extinfo = AndroidDlExtInfo {
        flags: ANDROID_DLEXT_FORCE_LOAD,
        ..AndroidDlExtInfo::default()
    };
    let handle = unsafe { android_dlopen_ext(path.as_ptr(), libc::RTLD_NOW, &extinfo) };
    if !handle.is_null() {
        return Some(LoadStatus::Opened {
            handle,
            method: "android_dlopen_ext(force)",
        });
    }

    errors.push(LoadError {
        method: "android_dlopen_ext(force)",
        error: dlerror_string(),
    });
    None
}

#[cfg(not(target_os = "android"))]
fn try_android_force_load(_path: &CStr, _errors: &mut Vec<LoadError>) -> Option<LoadStatus> {
    None
}

fn library_is_already_mapped(path: &str) -> bool {
    let Some(name) = crate::paths::basename(path) else {
        return false;
    };
    std::fs::read_to_string("/proc/self/maps")
        .map(|maps| {
            maps.lines()
                .any(|line| map_line_matches_library(line, path, name))
        })
        .unwrap_or(false)
}

fn map_line_matches_library(line: &str, path: &str, name: &str) -> bool {
    let Some(mapped_path) = line.split_whitespace().nth(5) else {
        return false;
    };
    if mapped_path.starts_with('[') {
        return mapped_path == path;
    }
    if path.starts_with('/') {
        if mapped_path.starts_with('/') {
            mapped_path == path
        } else {
            path.ends_with(mapped_path)
        }
    } else if mapped_path.starts_with('/') {
        mapped_path.ends_with(name)
    } else {
        mapped_path == name
    }
}

fn dlerror_string() -> String {
    let ptr = unsafe { dlerror() };
    if ptr.is_null() {
        return "unknown linker error".to_string();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_relative_library_by_mapped_basename() {
        let line = "7ac49c2000-7ac4a26000 r-xp 00000000 00:00 1245 /x/libgadget.so";

        assert!(map_line_matches_library(
            line,
            "libgadget.so",
            "libgadget.so"
        ));
    }

    #[test]
    fn matches_absolute_library_path_exactly() {
        let line = "7ac49c2000-7ac4a26000 r-xp 00000000 00:00 1245 /x/libgadget.so";

        assert!(map_line_matches_library(
            line,
            "/x/libgadget.so",
            "libgadget.so"
        ));
        assert!(!map_line_matches_library(
            line,
            "/y/libgadget.so",
            "libgadget.so"
        ));
    }
}
