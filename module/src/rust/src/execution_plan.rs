use std::os::fd::{AsRawFd, OwnedFd};

use crate::config::{ChildGatingConfig, ChildGatingMode, LibraryConfig, StagingMode, TargetConfig};
use crate::ffi::JInt;
use crate::paths::{is_absolute_path, openat_owned};
use crate::staging;

#[derive(Debug)]
pub(crate) struct ExecutionPlan {
    pub(crate) app_name: String,
    pub(crate) enabled: bool,
    pub(crate) start_up_delay_ms: u64,
    pub(crate) libraries: Vec<PlannedLibrary>,
    pub(crate) child_gating: ChildGatingPlan,
}

#[derive(Debug)]
pub(crate) struct ChildGatingPlan {
    pub(crate) enabled: bool,
    pub(crate) mode: ChildGatingMode,
    pub(crate) libraries: Vec<PlannedLibrary>,
}

impl Default for ChildGatingPlan {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ChildGatingMode::Freeze,
            libraries: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PlannedLibrary {
    pub(crate) label: String,
    pub(crate) source_path: String,
    pub(crate) load_path: String,
    pub(crate) remap_hints: Vec<String>,
    pub(crate) location: LibraryLocation,
}

#[derive(Debug)]
pub(crate) enum LibraryLocation {
    ModuleFd(OwnedFd),
    StagedPath(String),
    AbsolutePath(String),
}

pub(crate) fn build_execution_plan(
    module_dir_fd: libc::c_int,
    cfg: TargetConfig,
    app_data_dir: Option<&str>,
    uid: JInt,
    gid: JInt,
) -> Option<ExecutionPlan> {
    let mut plan = ExecutionPlan {
        app_name: cfg.app_name,
        enabled: cfg.enabled,
        start_up_delay_ms: cfg.start_up_delay_ms,
        libraries: resolve_libraries(module_dir_fd, &cfg.injected_libraries, "parent")?,
        child_gating: child_gating_plan(module_dir_fd, &cfg.child_gating)?,
    };

    if plan.enabled && cfg.staging == StagingMode::AppData {
        let Some(app_data_dir) = app_data_dir else {
            loge!(
                "cannot stage libraries without app data dir for {}",
                plan.app_name
            );
            return None;
        };
        if !staging::stage_plan(module_dir_fd, &mut plan, app_data_dir, uid, gid) {
            return None;
        }
    }

    Some(plan)
}

fn child_gating_plan(
    module_dir_fd: libc::c_int,
    cfg: &ChildGatingConfig,
) -> Option<ChildGatingPlan> {
    if !cfg.enabled {
        return Some(ChildGatingPlan::default());
    }

    Some(ChildGatingPlan {
        enabled: true,
        mode: cfg.mode,
        libraries: resolve_libraries(module_dir_fd, &cfg.injected_libraries, "child")?,
    })
}

fn resolve_libraries(
    module_dir_fd: libc::c_int,
    libraries: &[LibraryConfig],
    label_prefix: &str,
) -> Option<Vec<PlannedLibrary>> {
    libraries
        .iter()
        .enumerate()
        .map(|(index, library)| {
            resolve_library(module_dir_fd, library, format!("{label_prefix}[{index}]"))
        })
        .collect()
}

fn resolve_library(
    module_dir_fd: libc::c_int,
    library: &LibraryConfig,
    label: String,
) -> Option<PlannedLibrary> {
    if is_absolute_path(&library.path) {
        return Some(PlannedLibrary::new_absolute(label, library.path.clone()));
    }

    let fd = match openat_owned(
        module_dir_fd,
        &library.path,
        libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    ) {
        Ok(fd) => fd,
        Err(err) => {
            let library_display = library_for_log(&label, &library.path);
            loge!("failed to open module library {library_display}: {err}");
            return None;
        }
    };
    Some(PlannedLibrary::new_module_fd(
        label,
        library.path.clone(),
        fd,
    ))
}

impl PlannedLibrary {
    fn new_absolute(label: String, path: String) -> Self {
        Self {
            label,
            source_path: path.clone(),
            load_path: path.clone(),
            remap_hints: vec![path.clone()],
            location: LibraryLocation::AbsolutePath(path),
        }
    }

    fn new_module_fd(label: String, source_path: String, fd: OwnedFd) -> Self {
        let load_path = format!("/proc/self/fd/{}", fd.as_raw_fd());
        Self {
            label,
            source_path: source_path.clone(),
            load_path: load_path.clone(),
            remap_hints: vec![load_path, source_path],
            location: LibraryLocation::ModuleFd(fd),
        }
    }

    pub(crate) fn set_staged_path(&mut self, staged_path: String) {
        self.load_path = staged_path.clone();
        self.remap_hints = vec![staged_path.clone(), self.source_path.clone()];
        self.location = LibraryLocation::StagedPath(staged_path);
    }
}

fn library_for_log(label: &str, path: &str) -> String {
    if crate::logging::verbose_diagnostics() {
        format!("{label} ({path})")
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChildGatingConfig, StagingMode, TargetConfig};
    use crate::paths::{current_gid, current_uid, open_owned};
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::path::Path;
    use tempfile::tempdir;

    fn write(path: &Path, data: &[u8]) {
        fs::write(path, data).unwrap();
    }

    fn module_fd(dir: &Path) -> OwnedFd {
        open_owned(dir, libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY, 0).unwrap()
    }

    fn cfg(library: &str, staging: StagingMode) -> TargetConfig {
        TargetConfig {
            enabled: true,
            app_name: "pkg".to_string(),
            start_up_delay_ms: 25,
            staging,
            injected_libraries: vec![LibraryConfig::new(library.to_string())],
            child_gating: ChildGatingConfig::default(),
        }
    }

    #[test]
    fn resolves_relative_library_to_module_fd() {
        let module = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());

        let plan = build_execution_plan(
            module_fd.as_raw_fd(),
            cfg("libgadget.so", StagingMode::Disabled),
            None,
            0,
            0,
        )
        .unwrap();

        assert_eq!(plan.libraries[0].source_path, "libgadget.so");
        assert_eq!(plan.libraries[0].label, "parent[0]");
        assert!(plan.libraries[0].load_path.starts_with("/proc/self/fd/"));
        assert!(matches!(
            plan.libraries[0].location,
            LibraryLocation::ModuleFd(_)
        ));
    }

    #[test]
    fn remap_hints_include_staged_and_source_paths() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());

        let plan = build_execution_plan(
            module_fd.as_raw_fd(),
            cfg("libgadget.so", StagingMode::AppData),
            Some(app.path().to_str().unwrap()),
            current_uid() as JInt,
            current_gid() as JInt,
        )
        .unwrap();

        assert!(matches!(
            plan.libraries[0].location,
            LibraryLocation::StagedPath(_)
        ));
        assert_eq!(plan.libraries[0].label, "parent[0]");
        assert_eq!(plan.libraries[0].remap_hints.len(), 2);
        assert!(plan.libraries[0].remap_hints[0].contains("files/.zygiskfrida/0000-"));
        assert_eq!(plan.libraries[0].remap_hints[1], "libgadget.so");
    }

    #[test]
    fn labels_parent_and_child_libraries_stably() {
        let module = tempdir().unwrap();
        write(&module.path().join("libparent.so"), b"parent");
        write(&module.path().join("libchild.so"), b"child");
        let module_fd = module_fd(module.path());
        let mut cfg = cfg("libparent.so", StagingMode::Disabled);
        cfg.child_gating = ChildGatingConfig {
            enabled: true,
            mode: ChildGatingMode::Inject,
            injected_libraries: vec![LibraryConfig::new("libchild.so".to_string())],
        };

        let plan = build_execution_plan(module_fd.as_raw_fd(), cfg, None, 0, 0).unwrap();

        assert_eq!(plan.libraries[0].label, "parent[0]");
        assert_eq!(plan.child_gating.libraries[0].label, "child[0]");
    }
}
