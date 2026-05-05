use std::os::fd::{AsRawFd, OwnedFd};

use libc::c_int;
use serde::Deserialize;

use crate::paths::{is_absolute_path, module_fd_path, openat_owned, read_text_file};

#[derive(Debug)]
pub(crate) struct LibraryConfig {
    pub(crate) path: String,
    pub(crate) source_path: String,
    pub(crate) fd: Option<OwnedFd>,
}

impl LibraryConfig {
    pub(crate) fn new(path: String) -> Self {
        Self {
            source_path: path.clone(),
            path,
            fd: None,
        }
    }

    pub(crate) fn new_with_source(path: String, source_path: String) -> Self {
        Self {
            path,
            source_path,
            fd: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ChildGatingConfig {
    pub(crate) enabled: bool,
    pub(crate) mode: ChildGatingMode,
    pub(crate) injected_libraries: Vec<LibraryConfig>,
}

impl Default for ChildGatingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ChildGatingMode::Freeze,
            injected_libraries: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct TargetConfig {
    pub(crate) enabled: bool,
    pub(crate) app_name: String,
    pub(crate) start_up_delay_ms: u64,
    pub(crate) stage_libraries_in_app_data: bool,
    pub(crate) injected_libraries: Vec<LibraryConfig>,
    pub(crate) child_gating: ChildGatingConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChildGatingMode {
    Freeze,
    Kill,
    Inject,
}

impl<'de> Deserialize<'de> for ChildGatingMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "freeze" => Ok(Self::Freeze),
            "kill" => Ok(Self::Kill),
            "inject" => Ok(Self::Inject),
            other => Err(serde::de::Error::custom(format!(
                "invalid child_gating.mode {other}"
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigRootJson {
    targets: Vec<TargetConfigJson>,
}

#[derive(Debug, Deserialize)]
struct TargetConfigJson {
    app_name: String,
    enabled: bool,
    start_up_delay_ms: u64,
    #[serde(default)]
    stage_libraries_in_app_data: bool,
    injected_libraries: Vec<LibraryConfigJson>,
    #[serde(default)]
    child_gating: Option<ChildGatingConfigJson>,
}

#[derive(Debug, Deserialize)]
struct ChildGatingConfigJson {
    enabled: bool,
    mode: ChildGatingMode,
    #[serde(default)]
    injected_libraries: Vec<LibraryConfigJson>,
}

#[derive(Debug, Deserialize)]
struct LibraryConfigJson {
    path: String,
}

pub(crate) fn load_config(module_dir_fd: c_int, app_name: &str) -> Option<TargetConfig> {
    if module_dir_fd < 0 {
        return None;
    }
    match load_advanced_config(module_dir_fd, app_name) {
        AdvancedConfigResult::Matched(config) => Some(config),
        AdvancedConfigResult::NoConfigOrNoMatch => load_simple_config(module_dir_fd, app_name),
        AdvancedConfigResult::Invalid => None,
    }
}

enum AdvancedConfigResult {
    Matched(TargetConfig),
    NoConfigOrNoMatch,
    Invalid,
}

fn load_advanced_config(module_dir_fd: c_int, app_name: &str) -> AdvancedConfigResult {
    let config_path = module_fd_path(module_dir_fd, "config.json");
    let config = match read_text_file(&config_path) {
        Ok(config) => config,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return AdvancedConfigResult::NoConfigOrNoMatch;
        }
        Err(err) => {
            loge!("failed to read config.json: {err}");
            return AdvancedConfigResult::Invalid;
        }
    };
    let root: ConfigRootJson = match serde_json::from_str(&config) {
        Ok(root) => root,
        Err(err) => {
            loge!("config is not a valid json file: {err}");
            return AdvancedConfigResult::Invalid;
        }
    };

    for target in root.targets {
        let mut target = target_from_json(target);
        if target.app_name == app_name {
            if target.enabled && !resolve_library_paths(module_dir_fd, &mut target) {
                return AdvancedConfigResult::Invalid;
            }
            return AdvancedConfigResult::Matched(target);
        }
    }
    AdvancedConfigResult::NoConfigOrNoMatch
}

fn load_simple_config(module_dir_fd: c_int, app_name: &str) -> Option<TargetConfig> {
    let config = match read_text_file(&module_fd_path(module_dir_fd, "target_packages")) {
        Ok(config) => config,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            loge!("failed to read target_packages: {err}");
            return None;
        }
    };
    for line in config.lines().filter(|line| !line.is_empty()) {
        let Some((target_app_name, start_up_delay_ms)) = split_legacy_target(line) else {
            continue;
        };
        if target_app_name != app_name {
            continue;
        }

        let injected_libraries = parse_injected_libraries(module_dir_fd)?;
        let mut cfg = TargetConfig {
            enabled: true,
            app_name: target_app_name.to_string(),
            start_up_delay_ms,
            stage_libraries_in_app_data: false,
            injected_libraries,
            child_gating: ChildGatingConfig::default(),
        };
        if !resolve_library_paths(module_dir_fd, &mut cfg) {
            return None;
        }
        return Some(cfg);
    }
    None
}

fn target_from_json(target: TargetConfigJson) -> TargetConfig {
    let child_gating = target
        .child_gating
        .map(|child| ChildGatingConfig {
            enabled: child.enabled,
            mode: child.mode,
            injected_libraries: deserialize_libraries(child.injected_libraries),
        })
        .unwrap_or_default();

    TargetConfig {
        enabled: target.enabled,
        app_name: target.app_name,
        start_up_delay_ms: target.start_up_delay_ms,
        stage_libraries_in_app_data: target.stage_libraries_in_app_data,
        injected_libraries: deserialize_libraries(target.injected_libraries),
        child_gating,
    }
}

fn deserialize_libraries(libraries: Vec<LibraryConfigJson>) -> Vec<LibraryConfig> {
    libraries
        .into_iter()
        .map(|library| LibraryConfig::new(library.path))
        .collect()
}

fn split_legacy_target(line: &str) -> Option<(&str, u64)> {
    let (app_name, delay) = line.split_once(',').unwrap_or((line, "0"));
    if app_name.is_empty() {
        return None;
    }
    Some((app_name, delay.parse::<u64>().unwrap_or(0)))
}

fn parse_injected_libraries(module_dir_fd: c_int) -> Option<Vec<LibraryConfig>> {
    let config = match read_text_file(&module_fd_path(module_dir_fd, "injected_libraries")) {
        Ok(config) => config,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Some(vec![LibraryConfig::new("libgadget.so".to_string())]);
        }
        Err(err) => {
            loge!("failed to read injected_libraries: {err}");
            return None;
        }
    };

    Some(
        config
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| LibraryConfig::new(line.to_string()))
            .collect(),
    )
}

pub(crate) fn resolve_library_paths(module_dir_fd: c_int, cfg: &mut TargetConfig) -> bool {
    for library in &mut cfg.injected_libraries {
        if !resolve_library_path(module_dir_fd, library) {
            return false;
        }
    }
    if cfg.child_gating.enabled {
        for library in &mut cfg.child_gating.injected_libraries {
            if !resolve_library_path(module_dir_fd, library) {
                return false;
            }
        }
    }
    true
}

fn resolve_library_path(module_dir_fd: c_int, library: &mut LibraryConfig) -> bool {
    library.source_path = library.path.clone();
    if is_absolute_path(&library.path) {
        return true;
    }

    match openat_owned(
        module_dir_fd,
        &library.path,
        libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    ) {
        Ok(fd) => {
            library.path = format!("/proc/self/fd/{}", fd.as_raw_fd());
            library.fd = Some(fd);
            true
        }
        Err(err) => {
            loge!("failed to open module library {}: {err}", library.path);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::open_owned;
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::fd::OwnedFd;
    use std::path::Path;
    use tempfile::tempdir;

    fn write(path: &Path, data: &[u8]) {
        fs::write(path, data).unwrap();
    }

    fn module_fd(dir: &Path) -> OwnedFd {
        open_owned(dir, libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY, 0).unwrap()
    }

    #[test]
    fn parses_advanced_config() {
        let mut cfg = target_from_json(
            serde_json::from_str::<ConfigRootJson>(
                r#"{
                  "targets": [{
                    "app_name": "com.example.package",
                    "enabled": true,
                    "start_up_delay_ms": 123,
                    "stage_libraries_in_app_data": true,
                    "injected_libraries": [{"path": "libgadget.so"}],
                    "child_gating": {
                      "enabled": true,
                      "mode": "inject",
                      "injected_libraries": [{"path": "libgadget-child.so"}]
                    }
                  }]
                }"#,
            )
            .unwrap()
            .targets
            .remove(0),
        );

        assert!(cfg.enabled);
        assert_eq!(cfg.app_name, "com.example.package");
        assert_eq!(cfg.start_up_delay_ms, 123);
        assert!(cfg.stage_libraries_in_app_data);
        assert_eq!(cfg.injected_libraries.remove(0).path, "libgadget.so");
        assert_eq!(cfg.child_gating.mode, ChildGatingMode::Inject);
    }

    #[test]
    fn rejects_invalid_child_gating_mode() {
        let err = serde_json::from_str::<ConfigRootJson>(
            r#"{
              "targets": [{
                "app_name": "pkg",
                "enabled": true,
                "start_up_delay_ms": 0,
                "injected_libraries": [],
                "child_gating": {"enabled": true, "mode": "bad"}
              }]
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("invalid child_gating.mode"));
    }

    #[test]
    fn parses_legacy_simple_config() {
        assert_eq!(
            split_legacy_target("com.example.package,20000").unwrap(),
            ("com.example.package", 20000)
        );
        assert_eq!(
            split_legacy_target("com.example.package").unwrap(),
            ("com.example.package", 0)
        );
    }

    #[test]
    fn malformed_config_disables_target_safely() {
        let module = tempdir().unwrap();
        write(
            &module.path().join("config.json"),
            br#"{"targets": "not-array"}"#,
        );
        write(&module.path().join("target_packages"), b"pkg\n");
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());

        assert!(load_config(module_fd.as_raw_fd(), "pkg").is_none());
    }

    #[test]
    fn legacy_config_opens_relative_libraries() {
        let module = tempdir().unwrap();
        write(&module.path().join("target_packages"), b"pkg,25\n");
        write(&module.path().join("injected_libraries"), b"libgadget.so\n");
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());

        let cfg = load_config(module_fd.as_raw_fd(), "pkg").unwrap();

        assert_eq!(cfg.app_name, "pkg");
        assert_eq!(cfg.start_up_delay_ms, 25);
        assert!(cfg.injected_libraries[0].fd.is_some());
        assert!(cfg.injected_libraries[0].path.starts_with("/proc/self/fd/"));
    }
}
