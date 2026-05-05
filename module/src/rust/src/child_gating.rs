use std::ffi::{c_char, c_void};
use std::mem;
use std::os::fd::{AsRawFd, OwnedFd};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::config::ChildGatingMode;
use crate::execution_plan::{ChildGatingPlan, LibraryLocation, PlannedLibrary};
use crate::ffi::ApiTable;
use crate::inject;
use crate::paths::{c_string, current_pid, duplicate_fd, open_owned};

type SharedFd = std::sync::Arc<OwnedFd>;

struct ChildLibrary {
    label: String,
    load_path: String,
    remap_hints: Vec<String>,
    _fd: Option<SharedFd>,
}

struct ChildGatingState {
    mode: ChildGatingMode,
    libraries: Vec<ChildLibrary>,
}

static CHILD_GATING_STATE: AtomicPtr<ChildGatingState> = AtomicPtr::new(ptr::null_mut());
static ORIG_FORK: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static ORIG_VFORK: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

pub(crate) fn install(api: *mut ApiTable, cfg: &ChildGatingPlan) -> bool {
    if api.is_null() || !cfg.enabled {
        return false;
    }
    if !CHILD_GATING_STATE.load(Ordering::SeqCst).is_null() {
        loge!("[child_gating] hooks are already installed");
        return true;
    }

    let Ok(regex) = c_string(".*") else {
        return false;
    };
    let fork = c"fork";
    let vfork = c"vfork";

    let Some(api_ref) = api_table_from_ptr(api) else {
        return false;
    };
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
    register_plt_hook(
        register,
        regex.as_ptr(),
        fork.as_ptr(),
        fork_replacement as *mut c_void,
        &mut orig_fork,
    );
    register_plt_hook(
        register,
        regex.as_ptr(),
        vfork.as_ptr(),
        vfork_replacement as *mut c_void,
        &mut orig_vfork,
    );

    if !commit_plt_hooks(commit) {
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
            .libraries
            .iter()
            .map(duplicate_library_for_child)
            .collect(),
    }));
    if CHILD_GATING_STATE
        .compare_exchange(ptr::null_mut(), state, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        loge!("[child_gating] hooks were installed concurrently");
        unsafe {
            drop(Box::from_raw(state));
        }
        return true;
    }
    ORIG_FORK.store(orig_fork, Ordering::SeqCst);
    ORIG_VFORK.store(orig_vfork, Ordering::SeqCst);
    logi!(
        "[child_gating] child gating enabled for fork/vfork; clone and clone3 are unsupported by the current PLT-hook backend"
    );
    true
}

fn api_table_from_ptr<'a>(api: *mut ApiTable) -> Option<&'a ApiTable> {
    if api.is_null() {
        None
    } else {
        Some(unsafe { &*api })
    }
}

fn register_plt_hook(
    register: unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void, *mut *mut c_void),
    regex: *const c_char,
    symbol: *const c_char,
    replacement: *mut c_void,
    original: &mut *mut c_void,
) {
    unsafe {
        register(regex, symbol, replacement, original);
    }
}

fn commit_plt_hooks(commit: unsafe extern "C" fn() -> bool) -> bool {
    unsafe { commit() }
}

fn duplicate_library_for_child(library: &PlannedLibrary) -> ChildLibrary {
    let fd = match &library.location {
        LibraryLocation::ModuleFd(fd) => duplicate_fd(fd.as_raw_fd()).ok(),
        LibraryLocation::AbsolutePath(path) | LibraryLocation::StagedPath(path) => open_owned(
            std::path::Path::new(path),
            libc::O_RDONLY | libc::O_CLOEXEC,
            0,
        )
        .ok(),
    };

    let Some(fd) = fd else {
        loge!(
            "failed to duplicate child gating fd for {}",
            library_display(library)
        );
        return ChildLibrary {
            label: library.label.clone(),
            load_path: library.load_path.clone(),
            remap_hints: library.remap_hints.clone(),
            _fd: None,
        };
    };

    let fd = std::sync::Arc::new(fd);
    let load_path = format!("/proc/self/fd/{}", fd.as_raw_fd());
    let mut remap_hints = vec![load_path.clone()];
    remap_hints.extend(
        library
            .remap_hints
            .iter()
            .filter(|hint| hint.as_str() != load_path)
            .cloned(),
    );
    ChildLibrary {
        label: library.label.clone(),
        load_path,
        remap_hints,
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
    let parent_pid = current_pid();
    logi!("[child_gating][pid {parent_pid}] detected {name}");

    let child_pid = unsafe { original() };
    if child_pid != 0 {
        logi!("[child_gating][pid {parent_pid}] returning from forking {child_pid}");
        return child_pid;
    }

    let child_pid = current_pid();
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
                logi!(
                    "{log_context}Injecting {}",
                    child_library_for_log(&library.label, &library.load_path)
                );
                inject::inject_library_by_path(
                    &library.load_path,
                    &library.remap_hints,
                    &library.label,
                    &log_context,
                );
            }
        }
        None => {
            loge!("{log_context}child gating state missing");
        }
    }
    0
}

fn library_display(library: &PlannedLibrary) -> String {
    if crate::logging::verbose_diagnostics() {
        format!("{} ({})", library.label, library.load_path)
    } else {
        library.label.clone()
    }
}

fn child_library_for_log(label: &str, path: &str) -> String {
    if crate::logging::verbose_diagnostics() {
        format!("{label} ({path})")
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_plan::{LibraryLocation, PlannedLibrary};
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
        let load_path = format!("/proc/self/fd/{original_fd}");
        let library = PlannedLibrary {
            label: "child[0]".to_string(),
            load_path: load_path.clone(),
            source_path: "libgadget-child.so".to_string(),
            remap_hints: vec![load_path, "libgadget-child.so".to_string()],
            location: LibraryLocation::ModuleFd(fd),
        };

        let child_library = duplicate_library_for_child(&library);

        assert_ne!(
            child_library.load_path,
            format!("/proc/self/fd/{original_fd}")
        );
        assert!(child_library.load_path.starts_with("/proc/self/fd/"));
        assert!(child_library._fd.is_some());
    }

    #[test]
    fn child_gating_state_is_single_assignment() {
        let state = Box::into_raw(Box::new(ChildGatingState {
            mode: ChildGatingMode::Freeze,
            libraries: Vec::new(),
        }));
        assert!(
            CHILD_GATING_STATE
                .compare_exchange(ptr::null_mut(), state, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        );
        let second = Box::into_raw(Box::new(ChildGatingState {
            mode: ChildGatingMode::Kill,
            libraries: Vec::new(),
        }));
        assert!(
            CHILD_GATING_STATE
                .compare_exchange(ptr::null_mut(), second, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        );
        unsafe {
            drop(Box::from_raw(second));
            drop(Box::from_raw(
                CHILD_GATING_STATE.swap(ptr::null_mut(), Ordering::SeqCst),
            ));
        }
    }
}
