use std::ffi::c_void;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::config::{ChildGatingConfig, ChildGatingMode, LibraryConfig};
use crate::ffi::ApiTable;
use crate::inject;
use crate::paths::c_string;

type SharedFd = std::sync::Arc<OwnedFd>;

struct ChildLibrary {
    path: String,
    source_path: String,
    _fd: Option<SharedFd>,
}

struct ChildGatingState {
    mode: ChildGatingMode,
    libraries: Vec<ChildLibrary>,
}

static CHILD_GATING_STATE: AtomicPtr<ChildGatingState> = AtomicPtr::new(ptr::null_mut());
static ORIG_FORK: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static ORIG_VFORK: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

pub(crate) fn install(api: *mut ApiTable, cfg: &ChildGatingConfig) -> bool {
    if api.is_null() || !cfg.enabled {
        return false;
    }

    let Ok(regex) = c_string(".*") else {
        return false;
    };
    let fork = c"fork";
    let vfork = c"vfork";

    let api_ref = unsafe { &*api };
    let Some(register) = api_ref.plt_hook_register else {
        loge!("[child_gating] pltHookRegister unavailable");
        return false;
    };
    let Some(commit) = api_ref.plt_hook_commit else {
        loge!("[child_gating] pltHookCommit unavailable");
        return false;
    };

    let mut orig_fork = ptr::null_mut();
    let mut orig_vfork = ptr::null_mut();
    unsafe {
        register(
            regex.as_ptr(),
            fork.as_ptr(),
            fork_replacement as *mut c_void,
            &mut orig_fork,
        );
        register(
            regex.as_ptr(),
            vfork.as_ptr(),
            vfork_replacement as *mut c_void,
            &mut orig_vfork,
        );
    }

    if !unsafe { commit() } {
        loge!("[child_gating] failed to commit PLT hooks");
        return false;
    }
    if orig_fork.is_null() && orig_vfork.is_null() {
        loge!(
            "[child_gating] PLT hooks committed but no original fork/vfork pointers were returned"
        );
        return false;
    }

    let state = Box::into_raw(Box::new(ChildGatingState {
        mode: cfg.mode,
        libraries: cfg
            .injected_libraries
            .iter()
            .map(duplicate_library_for_child)
            .collect(),
    }));
    CHILD_GATING_STATE.store(state, Ordering::SeqCst);
    ORIG_FORK.store(orig_fork, Ordering::SeqCst);
    ORIG_VFORK.store(orig_vfork, Ordering::SeqCst);
    logi!("[child_gating] child gating enabled");
    true
}

fn duplicate_library_for_child(library: &LibraryConfig) -> ChildLibrary {
    let Some(source_fd) = &library.fd else {
        return ChildLibrary {
            path: library.path.clone(),
            source_path: library.source_path.clone(),
            _fd: None,
        };
    };

    let dup_fd = unsafe { libc::dup(source_fd.as_raw_fd()) };
    if dup_fd < 0 {
        loge!(
            "failed to duplicate child gating fd for {}: {}",
            library.path,
            std::io::Error::last_os_error()
        );
        return ChildLibrary {
            path: library.path.clone(),
            source_path: library.source_path.clone(),
            _fd: None,
        };
    }

    let fd = std::sync::Arc::new(unsafe { OwnedFd::from_raw_fd(dup_fd) });
    ChildLibrary {
        path: format!("/proc/self/fd/{}", fd.as_raw_fd()),
        source_path: library.source_path.clone(),
        _fd: Some(fd),
    }
}

unsafe extern "C" fn fork_replacement() -> libc::pid_t {
    unsafe { handle_fork(ORIG_FORK.load(Ordering::SeqCst), "fork") }
}

unsafe extern "C" fn vfork_replacement() -> libc::pid_t {
    unsafe { handle_fork(ORIG_VFORK.load(Ordering::SeqCst), "vfork") }
}

unsafe fn handle_fork(original_ptr: *mut c_void, name: &str) -> libc::pid_t {
    if original_ptr.is_null() {
        loge!("[child_gating] original {name} pointer missing");
        return -1;
    }

    let original: unsafe extern "C" fn() -> libc::pid_t = unsafe { mem::transmute(original_ptr) };
    let parent_pid = unsafe { libc::getpid() };
    logi!("[child_gating][pid {parent_pid}] detected {name}");

    let child_pid = unsafe { original() };
    if child_pid != 0 {
        logi!("[child_gating][pid {parent_pid}] returning from forking {child_pid}");
        return child_pid;
    }

    let child_pid = unsafe { libc::getpid() };
    let log_context = format!("[child_gating][pid {child_pid}] ");
    let state = CHILD_GATING_STATE.load(Ordering::SeqCst);
    let state = unsafe { state.as_ref() };

    match state {
        Some(ChildGatingState {
            mode: ChildGatingMode::Kill,
            ..
        }) => {
            logi!("{log_context}killing child process");
            unsafe { libc::_exit(0) };
        }
        Some(ChildGatingState {
            mode: ChildGatingMode::Freeze,
            ..
        }) => {
            logi!("{log_context}freezing child process");
            loop {
                unsafe {
                    libc::pause();
                }
            }
        }
        Some(ChildGatingState {
            mode: ChildGatingMode::Inject,
            libraries,
        }) => {
            for library in libraries {
                logi!("{log_context}Injecting {}", library.path);
                let library = LibraryConfig::new_with_source(
                    library.path.clone(),
                    library.source_path.clone(),
                );
                inject::inject_library(&library, &log_context);
            }
        }
        None => {
            loge!("{log_context}child gating state missing");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LibraryConfig;
    use crate::paths::open_owned;
    use std::fs;
    use std::os::fd::AsRawFd;
    use tempfile::tempdir;

    #[test]
    fn child_gating_path_uses_duplicate_fd() {
        let dir = tempdir().unwrap();
        let library_path = dir.path().join("libgadget-child.so");
        fs::write(&library_path, b"library").unwrap();
        let fd = open_owned(&library_path, libc::O_RDONLY | libc::O_CLOEXEC, 0).unwrap();
        let original_fd = fd.as_raw_fd();
        let library = LibraryConfig {
            path: format!("/proc/self/fd/{original_fd}"),
            source_path: "libgadget-child.so".to_string(),
            fd: Some(fd),
        };

        let child_library = duplicate_library_for_child(&library);

        assert_ne!(child_library.path, format!("/proc/self/fd/{original_fd}"));
        assert!(child_library.path.starts_with("/proc/self/fd/"));
        assert!(child_library._fd.is_some());
    }
}
