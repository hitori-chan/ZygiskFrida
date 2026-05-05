use std::collections::HashSet;

use serde::Deserialize;

use crate::paths::{basename, build_stage_names, is_absolute_path, module_fd_path, read_text_file};

pub(crate) const CONFIG_VERSION: u32 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LibraryConfig {
    pub(crate) path: String,
}

impl LibraryConfig {
    pub(crate) fn new(path: String) -> Self {
        Self { path }
    }
}

#[derive(Debug, Eq, PartialEq)]
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

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct TargetConfig {
    pub(crate) enabled: bool,
    pub(crate) app_name: String,
    pub(crate) start_up_delay_ms: u64,
    pub(crate) staging: StagingMode,
    pub(crate) injected_libraries: Vec<LibraryConfig>,
    pub(crate) child_gating: ChildGatingConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChildGatingMode {
    Freeze,
    Kill,
    Inject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StagingMode {
    AppData,
    Disabled,
}

impl StagingMode {
    pub(crate) fn uses_app_data(self) -> bool {
        matches!(self, Self::AppData)
    }
}

impl<'de> Deserialize<'de> for StagingMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "app_data" => Ok(Self::AppData),
            "disabled" => Ok(Self::Disabled),
            other => Err(serde::de::Error::custom(format!(
                "invalid staging mode {other}"
            ))),
        }
    }
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
#[serde(deny_unknown_fields)]
struct ConfigRootJson {
    config_version: u32,
    targets: Vec<TargetConfigJson>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetConfigJson {
    app_name: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    start_up_delay_ms: u64,
    #[serde(default = "default_staging")]
    staging: StagingMode,
    #[serde(default = "default_parent_libraries")]
    injected_libraries: Vec<LibraryConfigJson>,
    #[serde(default)]
    child_gating: Option<ChildGatingConfigJson>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildGatingConfigJson {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_child_gating_mode")]
    mode: ChildGatingMode,
    #[serde(default)]
    injected_libraries: Vec<LibraryConfigJson>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryConfigJson {
    path: String,
}

pub(crate) fn load_config(module_dir_fd: libc::c_int, app_name: &str) -> Option<TargetConfig> {
    if module_dir_fd < 0 {
        return None;
    }
    let config_path = module_fd_path(module_dir_fd, "config.json");
    let config = match read_text_file(&config_path) {
        Ok(config) => config,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            loge!("config.json not found; legacy target_packages config is no longer supported");
            return None;
        }
        Err(err) => {
            loge!("failed to read config.json: {err}");
            return None;
        }
    };
    let root: ConfigRootJson = match serde_json::from_str(&config) {
        Ok(root) => root,
        Err(err) => {
            loge!("config is not a valid json file: {err}");
            return None;
        }
    };

    if let Err(err) = validate_root(&root) {
        loge!("invalid config.json: {err}");
        return None;
    }

    root.targets
        .into_iter()
        .map(target_from_json)
        .find(|target| target.app_name == app_name)
}

fn default_enabled() -> bool {
    true
}

fn default_staging() -> StagingMode {
    StagingMode::AppData
}

fn default_child_gating_mode() -> ChildGatingMode {
    ChildGatingMode::Freeze
}

fn default_parent_libraries() -> Vec<LibraryConfigJson> {
    vec![LibraryConfigJson {
        path: "libgadget.so".to_string(),
    }]
}

fn target_from_json(target: TargetConfigJson) -> TargetConfig {
    TargetConfig {
        enabled: target.enabled,
        app_name: target.app_name,
        start_up_delay_ms: target.start_up_delay_ms,
        staging: target.staging,
        injected_libraries: deserialize_libraries(target.injected_libraries),
        child_gating: target
            .child_gating
            .map(|child| ChildGatingConfig {
                enabled: child.enabled,
                mode: child.mode,
                injected_libraries: deserialize_libraries(child.injected_libraries),
            })
            .unwrap_or_default(),
    }
}

fn deserialize_libraries(libraries: Vec<LibraryConfigJson>) -> Vec<LibraryConfig> {
    libraries
        .into_iter()
        .map(|library| LibraryConfig::new(library.path))
        .collect()
}

fn validate_root(root: &ConfigRootJson) -> Result<(), String> {
    if root.config_version != CONFIG_VERSION {
        return Err(format!(
            "unsupported config_version {}; expected {CONFIG_VERSION}",
            root.config_version
        ));
    }

    let mut app_names = HashSet::new();
    for target in &root.targets {
        validate_target(target)?;
        if !app_names.insert(target.app_name.as_str()) {
            return Err(format!("duplicate target {}", target.app_name));
        }
    }
    Ok(())
}

fn validate_target(target: &TargetConfigJson) -> Result<(), String> {
    if target.app_name.trim().is_empty() {
        return Err("target app_name must not be empty".to_string());
    }
    validate_libraries("injected_libraries", &target.injected_libraries)?;

    if let Some(child) = &target.child_gating {
        if child.enabled
            && child.mode == ChildGatingMode::Inject
            && child.injected_libraries.is_empty()
        {
            return Err(format!(
                "target {} child_gating inject mode requires injected_libraries",
                target.app_name
            ));
        }
        if !child.injected_libraries.is_empty() {
            validate_libraries("child_gating.injected_libraries", &child.injected_libraries)?;
        }
    }

    if target.staging.uses_app_data() {
        validate_unique_stage_names(target)?;
    }
    Ok(())
}

fn validate_libraries(label: &str, libraries: &[LibraryConfigJson]) -> Result<(), String> {
    if libraries.is_empty() {
        return Err(format!("{label} must not be empty"));
    }

    let mut paths = HashSet::new();
    for library in libraries {
        validate_library_path(label, &library.path)?;
        if !paths.insert(library.path.as_str()) {
            return Err(format!("duplicate {label} path {}", library.path));
        }
    }
    Ok(())
}

fn validate_library_path(label: &str, path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err(format!("{label} path must not be empty"));
    }
    if path.as_bytes().contains(&0) {
        return Err(format!("{label} path contains NUL byte"));
    }
    if !is_absolute_path(path) && (path.starts_with("../") || path.contains("/../")) {
        return Err(format!("{label} path must not escape the module directory"));
    }
    if basename(path).is_none() {
        return Err(format!("{label} path has no valid basename: {path}"));
    }
    Ok(())
}

fn validate_unique_stage_names(target: &TargetConfigJson) -> Result<(), String> {
    let mut names = HashSet::new();
    for (index, library) in target.injected_libraries.iter().enumerate() {
        insert_stage_names(&mut names, &library.path, "parent", index)?;
    }
    if let Some(child) = &target.child_gating
        && child.enabled
    {
        for (index, library) in child.injected_libraries.iter().enumerate() {
            insert_stage_names(&mut names, &library.path, "child", 0x4000 + index)?;
        }
    }
    Ok(())
}

fn insert_stage_names(
    names: &mut HashSet<String>,
    path: &str,
    role: &str,
    index: usize,
) -> Result<(), String> {
    let Some((library_name, config_name)) = build_stage_names(path, role, index) else {
        return Err(format!("failed to build staged name for {path}"));
    };
    if !names.insert(library_name.clone()) {
        return Err(format!("duplicate staged name {library_name}"));
    }
    if !names.insert(config_name.clone()) {
        return Err(format!("duplicate staged name {config_name}"));
    }
    Ok(())
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

    fn parse_root(json: &str) -> ConfigRootJson {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn parses_v2_config() {
        let mut cfg = target_from_json(
            parse_root(
                r#"{
                  "config_version": 2,
                  "targets": [{
                    "app_name": "com.example.package",
                    "enabled": true,
                    "start_up_delay_ms": 123,
                    "staging": "app_data",
                    "injected_libraries": [{"path": "libgadget.so"}],
                    "child_gating": {
                      "enabled": true,
                      "mode": "inject",
                      "injected_libraries": [{"path": "libgadget-child.so"}]
                    }
                  }]
                }"#,
            )
            .targets
            .remove(0),
        );

        assert!(cfg.enabled);
        assert_eq!(cfg.app_name, "com.example.package");
        assert_eq!(cfg.start_up_delay_ms, 123);
        assert_eq!(cfg.staging, StagingMode::AppData);
        assert_eq!(cfg.injected_libraries.remove(0).path, "libgadget.so");
        assert_eq!(cfg.child_gating.mode, ChildGatingMode::Inject);
    }

    #[test]
    fn defaults_to_gadget_with_app_data_staging() {
        let target = parse_root(
            r#"{
              "config_version": 2,
              "targets": [{"app_name": "pkg"}]
            }"#,
        )
        .targets
        .remove(0);

        validate_target(&target).unwrap();
        let cfg = target_from_json(target);
        assert!(cfg.enabled);
        assert_eq!(cfg.staging, StagingMode::AppData);
        assert_eq!(cfg.injected_libraries[0].path, "libgadget.so");
    }

    #[test]
    fn rejects_invalid_child_gating_mode() {
        let err = serde_json::from_str::<ConfigRootJson>(
            r#"{
              "config_version": 2,
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
    fn rejects_legacy_config_version() {
        let root = parse_root(
            r#"{
              "config_version": 1,
              "targets": [{"app_name": "pkg"}]
            }"#,
        );

        assert!(validate_root(&root).unwrap_err().contains("unsupported"));
    }

    #[test]
    fn rejects_unknown_v1_staging_field() {
        let err = serde_json::from_str::<ConfigRootJson>(
            r#"{
              "config_version": 2,
              "targets": [{
                "app_name": "pkg",
                "stage_libraries_in_app_data": true
              }]
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("unknown field"));
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
    fn legacy_simple_config_is_not_loaded() {
        let module = tempdir().unwrap();
        write(&module.path().join("target_packages"), b"pkg,25\n");
        write(&module.path().join("injected_libraries"), b"libgadget.so\n");
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());

        assert!(load_config(module_fd.as_raw_fd(), "pkg").is_none());
    }

    #[test]
    fn loads_matching_v2_target_without_resolving_runtime_paths() {
        let module = tempdir().unwrap();
        write(
            &module.path().join("config.json"),
            br#"{
              "config_version": 2,
              "targets": [{
                "app_name": "pkg",
                "start_up_delay_ms": 25,
                "staging": "disabled",
                "injected_libraries": [{"path": "libgadget.so"}]
              }]
            }"#,
        );
        let module_fd = module_fd(module.path());

        let cfg = load_config(module_fd.as_raw_fd(), "pkg").unwrap();

        assert_eq!(cfg.app_name, "pkg");
        assert_eq!(cfg.start_up_delay_ms, 25);
        assert_eq!(cfg.staging, StagingMode::Disabled);
        assert_eq!(cfg.injected_libraries[0].path, "libgadget.so");
    }

    #[test]
    fn validates_empty_app_name_duplicate_targets_and_empty_libraries() {
        let empty_name = parse_root(
            r#"{
              "config_version": 2,
              "targets": [{"app_name": ""}]
            }"#,
        );
        assert!(validate_root(&empty_name).unwrap_err().contains("app_name"));

        let duplicate = parse_root(
            r#"{
              "config_version": 2,
              "targets": [{"app_name": "pkg"}, {"app_name": "pkg"}]
            }"#,
        );
        assert!(
            validate_root(&duplicate)
                .unwrap_err()
                .contains("duplicate target")
        );

        let empty_libraries = parse_root(
            r#"{
              "config_version": 2,
              "targets": [{"app_name": "pkg", "injected_libraries": []}]
            }"#,
        );
        assert!(
            validate_root(&empty_libraries)
                .unwrap_err()
                .contains("must not be empty")
        );
    }

    #[test]
    fn validates_bad_paths_duplicate_libraries_and_child_inject_libraries() {
        let bad_path = parse_root(
            r#"{
              "config_version": 2,
              "targets": [{
                "app_name": "pkg",
                "injected_libraries": [{"path": "../libgadget.so"}]
              }]
            }"#,
        );
        assert!(validate_root(&bad_path).unwrap_err().contains("escape"));

        let duplicate_library = parse_root(
            r#"{
              "config_version": 2,
              "targets": [{
                "app_name": "pkg",
                "injected_libraries": [{"path": "liba.so"}, {"path": "liba.so"}]
              }]
            }"#,
        );
        assert!(
            validate_root(&duplicate_library)
                .unwrap_err()
                .contains("duplicate")
        );

        let child_inject_without_libraries = parse_root(
            r#"{
              "config_version": 2,
              "targets": [{
                "app_name": "pkg",
                "child_gating": {"enabled": true, "mode": "inject"}
              }]
            }"#,
        );
        assert!(
            validate_root(&child_inject_without_libraries)
                .unwrap_err()
                .contains("requires")
        );
    }
}
