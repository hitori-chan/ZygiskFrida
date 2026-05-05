use std::ffi::{CStr, c_char, c_int, c_void};

#[derive(Debug)]
pub(crate) enum LoadStatus {
    Opened {
        handle: *mut c_void,
        method: &'static str,
    },
    AlreadyMapped,
}

#[derive(Debug)]
pub(crate) struct LoadError {
    pub(crate) method: &'static str,
    pub(crate) error: String,
}

trait LoaderBackend {
    fn already_mapped(&self, display_path: &str) -> bool;
    fn private_linker_force_load(&self, path: &CStr) -> Result<*mut c_void, String>;
    fn android_force_load(&self, path: &CStr) -> Result<*mut c_void, String>;
    fn dlopen(&self, path: &CStr) -> Result<*mut c_void, String>;
}

struct SystemLoader;

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
    load_with_backend(path, display_path, &SystemLoader)
}

fn load_with_backend(
    path: &CStr,
    display_path: &str,
    backend: &impl LoaderBackend,
) -> Result<LoadStatus, Vec<LoadError>> {
    if backend.already_mapped(display_path) {
        return Ok(LoadStatus::AlreadyMapped);
    }

    let mut errors = Vec::new();
    match backend.private_linker_force_load(path) {
        Ok(handle) if !handle.is_null() => {
            return Ok(LoadStatus::Opened {
                handle,
                method: "private linker force-load",
            });
        }
        Ok(_) => errors.push(LoadError {
            method: "private linker force-load",
            error: "returned null handle".to_string(),
        }),
        Err(error) => errors.push(LoadError {
            method: "private linker force-load",
            error,
        }),
    }

    match backend.android_force_load(path) {
        Ok(handle) if !handle.is_null() => {
            return Ok(LoadStatus::Opened {
                handle,
                method: "android_dlopen_ext(force)",
            });
        }
        Ok(_) => errors.push(LoadError {
            method: "android_dlopen_ext(force)",
            error: "returned null handle".to_string(),
        }),
        Err(error) => errors.push(LoadError {
            method: "android_dlopen_ext(force)",
            error,
        }),
    }

    match backend.dlopen(path) {
        Ok(handle) if !handle.is_null() => Ok(LoadStatus::Opened {
            handle,
            method: "dlopen",
        }),
        Ok(_) => {
            errors.push(LoadError {
                method: "dlopen",
                error: "returned null handle".to_string(),
            });
            Err(errors)
        }
        Err(error) => {
            errors.push(LoadError {
                method: "dlopen",
                error,
            });
            Err(errors)
        }
    }
}

impl LoaderBackend for SystemLoader {
    fn already_mapped(&self, display_path: &str) -> bool {
        library_is_already_mapped(display_path)
    }

    fn private_linker_force_load(&self, path: &CStr) -> Result<*mut c_void, String> {
        system_private_linker_force_load(path)
    }

    fn android_force_load(&self, path: &CStr) -> Result<*mut c_void, String> {
        system_android_force_load(path)
    }

    fn dlopen(&self, path: &CStr) -> Result<*mut c_void, String> {
        let handle = unsafe { dlopen(path.as_ptr(), libc::RTLD_NOW) };
        if handle.is_null() {
            Err(dlerror_string())
        } else {
            Ok(handle)
        }
    }
}

#[cfg(target_os = "android")]
fn system_private_linker_force_load(path: &CStr) -> Result<*mut c_void, String> {
    crate::android_linker::force_dlopen(path).map_err(|error| error.to_string())
}

#[cfg(not(target_os = "android"))]
fn system_private_linker_force_load(_path: &CStr) -> Result<*mut c_void, String> {
    Err("private linker force-load is only available on Android".to_string())
}

#[cfg(target_os = "android")]
fn system_android_force_load(path: &CStr) -> Result<*mut c_void, String> {
    let extinfo = AndroidDlExtInfo {
        flags: ANDROID_DLEXT_FORCE_LOAD,
        ..AndroidDlExtInfo::default()
    };
    let handle = unsafe { android_dlopen_ext(path.as_ptr(), libc::RTLD_NOW, &extinfo) };
    if handle.is_null() {
        Err(dlerror_string())
    } else {
        Ok(handle)
    }
}

#[cfg(not(target_os = "android"))]
fn system_android_force_load(_path: &CStr) -> Result<*mut c_void, String> {
    Err("android_dlopen_ext is only available on Android".to_string())
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
    use std::cell::RefCell;
    use std::ptr::NonNull;

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

    struct MockLoader {
        calls: RefCell<Vec<&'static str>>,
        already_mapped: bool,
        private_result: Result<*mut c_void, String>,
        android_result: Result<*mut c_void, String>,
        dlopen_result: Result<*mut c_void, String>,
    }

    impl Default for MockLoader {
        fn default() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                already_mapped: false,
                private_result: Err("private failed".to_string()),
                android_result: Err("android failed".to_string()),
                dlopen_result: Err("dlopen failed".to_string()),
            }
        }
    }

    impl LoaderBackend for MockLoader {
        fn already_mapped(&self, _display_path: &str) -> bool {
            self.calls.borrow_mut().push("already_mapped");
            self.already_mapped
        }

        fn private_linker_force_load(&self, _path: &CStr) -> Result<*mut c_void, String> {
            self.calls.borrow_mut().push("private");
            self.private_result.clone()
        }

        fn android_force_load(&self, _path: &CStr) -> Result<*mut c_void, String> {
            self.calls.borrow_mut().push("android");
            self.android_result.clone()
        }

        fn dlopen(&self, _path: &CStr) -> Result<*mut c_void, String> {
            self.calls.borrow_mut().push("dlopen");
            self.dlopen_result.clone()
        }
    }

    fn handle(value: usize) -> *mut c_void {
        NonNull::<u8>::new(value as *mut u8)
            .unwrap()
            .as_ptr()
            .cast()
    }

    #[test]
    fn loader_short_circuits_already_mapped_before_force_loads() {
        let loader = MockLoader {
            already_mapped: true,
            ..MockLoader::default()
        };
        let path = c"/x/libgadget.so";

        assert!(matches!(
            load_with_backend(path, "/x/libgadget.so", &loader).unwrap(),
            LoadStatus::AlreadyMapped
        ));
        assert_eq!(&*loader.calls.borrow(), &["already_mapped"]);
    }

    #[test]
    fn loader_uses_private_android_then_dlopen_order() {
        let loader = MockLoader {
            private_result: Err("private failed".to_string()),
            android_result: Err("android failed".to_string()),
            dlopen_result: Ok(handle(1)),
            ..MockLoader::default()
        };
        let path = c"/x/libgadget.so";

        let status = load_with_backend(path, "/x/libgadget.so", &loader).unwrap();
        assert!(matches!(
            status,
            LoadStatus::Opened {
                method: "dlopen",
                ..
            }
        ));
        assert_eq!(
            &*loader.calls.borrow(),
            &["already_mapped", "private", "android", "dlopen"]
        );
    }

    #[test]
    fn loader_aggregates_stage_errors() {
        let loader = MockLoader {
            private_result: Err("private failed".to_string()),
            android_result: Err("android failed".to_string()),
            dlopen_result: Err("dlopen failed".to_string()),
            ..MockLoader::default()
        };
        let path = c"/x/libgadget.so";

        let errors = load_with_backend(path, "/x/libgadget.so", &loader).unwrap_err();
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].method, "private linker force-load");
        assert_eq!(errors[1].method, "android_dlopen_ext(force)");
        assert_eq!(errors[2].method, "dlopen");
    }
}
