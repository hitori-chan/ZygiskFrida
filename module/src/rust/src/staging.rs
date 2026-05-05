use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use libc::c_int;

use crate::execution_plan::{ExecutionPlan, LibraryLocation, PlannedLibrary};
use crate::ffi::JInt;
use crate::paths::{
    basename, build_stage_names, c_string, current_pid, dirname, duplicate_fd, is_absolute_path,
    last_errno, open_owned, openat_owned, sidecar_name_for_library,
};

pub(crate) const DEFAULT_GADGET_CONFIG: &[u8] = br#"{
  "interaction": {
    "type": "listen",
    "address": "127.0.0.1",
    "port": 27042,
    "on_port_conflict": "pick-next",
    "on_load": "resume"
  }
}
"#;

pub(crate) fn stage_plan(
    module_dir_fd: c_int,
    plan: &mut ExecutionPlan,
    app_data_dir: &str,
    uid: JInt,
    gid: JInt,
) -> bool {
    if module_dir_fd < 0 || app_data_dir.is_empty() {
        loge!("cannot stage libraries without module dir fd and app data dir");
        return false;
    }

    let files_dir = Path::new(app_data_dir).join("files");
    let stage_dir = files_dir.join(".zygiskfrida");
    if !ensure_app_files_dir(&files_dir, "app files dir", 0o700, uid, gid)
        || !ensure_owned_dir(&stage_dir, "staging dir", 0o700, uid, gid)
    {
        return false;
    }
    if !clean_legacy_generated_files(&files_dir) {
        return false;
    }

    let mut manifest = HashSet::new();
    let mut context = StageContext {
        module_dir_fd,
        stage_dir: &stage_dir,
        uid,
        gid,
        manifest: &mut manifest,
    };
    if !stage_library_list(&mut plan.libraries, "parent", 0, &mut context) {
        return false;
    }
    if plan.child_gating.enabled
        && !stage_library_list(
            &mut plan.child_gating.libraries,
            "child",
            0x4000,
            &mut context,
        )
    {
        return false;
    }

    clean_stale_generated_files(&stage_dir, &manifest)
}

struct StageContext<'a> {
    module_dir_fd: c_int,
    stage_dir: &'a Path,
    uid: JInt,
    gid: JInt,
    manifest: &'a mut HashSet<String>,
}

fn stage_library_list(
    libraries: &mut [PlannedLibrary],
    role: &str,
    index_offset: usize,
    context: &mut StageContext<'_>,
) -> bool {
    for (index, library) in libraries.iter_mut().enumerate() {
        let Some((library_name, config_name)) =
            build_stage_names(&library.source_path, role, index_offset + index)
        else {
            loge!(
                "failed to build staged names for {}",
                library_display(library)
            );
            return false;
        };

        context.manifest.insert(library_name.clone());

        let staged_path = context.stage_dir.join(&library_name);
        let src_fd = match open_library_source(library) {
            Ok(fd) => fd,
            Err(err) => {
                loge!(
                    "failed to open source library {} for staging: {err}",
                    library_display(library)
                );
                return false;
            }
        };
        if !copy_fd_to_path(
            src_fd.as_raw_fd(),
            &staged_path,
            &format!("{} staged library", library.label),
            context.uid,
            context.gid,
            0o700,
        ) {
            return false;
        }

        let staged_config_path = context.stage_dir.join(config_name);
        match open_library_sidecar(context.module_dir_fd, library) {
            Ok(sidecar_fd) => {
                context
                    .manifest
                    .insert(staged_file_name(&staged_config_path));
                if !copy_fd_to_path(
                    sidecar_fd.as_raw_fd(),
                    &staged_config_path,
                    &format!("{} staged sidecar", library.label),
                    context.uid,
                    context.gid,
                    0o600,
                ) {
                    return false;
                }
            }
            Err(_)
                if basename(&library.source_path)
                    .is_some_and(|name| name.contains("libgadget")) =>
            {
                logi!(
                    "no gadget sidecar config found for {}, writing default listen config",
                    library.source_path
                );
                context
                    .manifest
                    .insert(staged_file_name(&staged_config_path));
                if !write_default_gadget_config(
                    &staged_config_path,
                    &format!("{} staged sidecar", library.label),
                    context.uid,
                    context.gid,
                ) {
                    return false;
                }
            }
            Err(_) => {}
        }

        library.set_staged_path(staged_path.to_string_lossy().into_owned());
    }
    true
}

fn clean_stale_generated_files(stage_dir: &Path, manifest: &HashSet<String>) -> bool {
    let entries = match fs::read_dir(stage_dir) {
        Ok(entries) => entries,
        Err(err) => {
            loge!(
                "failed to list staged dir {}: {err}",
                log_path("staging dir", stage_dir)
            );
            return false;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                loge!("failed to read staged dir entry: {err}");
                return false;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_generated_stage_name(name) || manifest.contains(name) {
            continue;
        }
        if let Err(err) = fs::remove_file(entry.path()) {
            loge!(
                "failed to remove stale staged file {}: {err}",
                log_path("stale staged file", &entry.path())
            );
            return false;
        }
    }
    true
}

fn clean_legacy_generated_files(files_dir: &Path) -> bool {
    let legacy_dir = files_dir.join("zygiskfrida");
    let entries = match fs::read_dir(&legacy_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return true,
        Err(err) => {
            loge!(
                "failed to list legacy staged dir {}: {err}",
                log_path("legacy staged dir", &legacy_dir)
            );
            return false;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                loge!("failed to read legacy staged dir entry: {err}");
                return false;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_legacy_generated_stage_name(name) {
            continue;
        }
        if let Err(err) = fs::remove_file(entry.path()) {
            loge!(
                "failed to remove legacy staged file {}: {err}",
                log_path("legacy staged file", &entry.path())
            );
            return false;
        }
    }
    true
}

fn is_generated_stage_name(name: &str) -> bool {
    let Some((prefix, rest)) = name.split_once('-') else {
        return false;
    };
    prefix.len() == 4
        && prefix.bytes().all(|b| b.is_ascii_hexdigit())
        && (is_generated_library_suffix(rest) || is_generated_sidecar_suffix(rest))
}

fn is_generated_library_suffix(rest: &str) -> bool {
    rest.len() == 19
        && rest.ends_with(".so")
        && rest[..16].bytes().all(|b| b.is_ascii_hexdigit())
        && rest[..16].bytes().all(|b| !b.is_ascii_uppercase())
}

fn is_generated_sidecar_suffix(rest: &str) -> bool {
    rest.len() == 26
        && rest.ends_with(".config.so")
        && rest[..16].bytes().all(|b| b.is_ascii_hexdigit())
        && rest[..16].bytes().all(|b| !b.is_ascii_uppercase())
}

fn is_legacy_generated_stage_name(name: &str) -> bool {
    let Some((prefix, rest)) = name.split_once('-') else {
        return false;
    };
    prefix.len() == 4 && !rest.is_empty() && prefix.bytes().all(|b| b.is_ascii_hexdigit())
}

fn ensure_app_files_dir(path: &Path, label: &str, mode: u32, uid: JInt, gid: JInt) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                loge!("{} exists but is not a directory", log_path(label, path));
                return false;
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Err(err) = fs::create_dir(path) {
                loge!("failed to create {}: {err}", log_path(label, path));
                return false;
            }
        }
        Err(err) => {
            loge!("failed to stat {}: {err}", log_path(label, path));
            return false;
        }
    }
    chown_chmod(path, label, mode, uid, gid)
}

fn ensure_owned_dir(path: &Path, label: &str, mode: u32, uid: JInt, gid: JInt) -> bool {
    if let Err(err) = fs::create_dir(path)
        && err.kind() != std::io::ErrorKind::AlreadyExists
    {
        loge!("failed to create {}: {err}", log_path(label, path));
        return false;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            loge!("{} exists but is not a directory", log_path(label, path));
            return false;
        }
        Err(err) => {
            loge!("failed to stat {}: {err}", log_path(label, path));
            return false;
        }
    }
    chown_chmod(path, label, mode, uid, gid)
}

fn chown_chmod(path: &Path, label: &str, mode: u32, uid: JInt, gid: JInt) -> bool {
    let Ok(path) = c_string(path.to_string_lossy().as_ref()) else {
        loge!("path contains NUL byte");
        return false;
    };
    if chown_path(&path, uid, gid) != 0 {
        loge!("failed to chown {label}: {}", last_errno());
        return false;
    }
    if chmod_path(&path, mode) != 0 {
        loge!("failed to chmod {label}: {}", last_errno());
        return false;
    }
    true
}

fn copy_fd_to_path(
    src_fd: c_int,
    dst_path: &Path,
    label: &str,
    uid: JInt,
    gid: JInt,
    mode: u32,
) -> bool {
    if seek_start(src_fd).is_err() {
        loge!("failed to seek source for {label}: {}", last_errno());
        return false;
    }

    let tmp_path = temp_path_for(dst_path);
    let _ = fs::remove_file(&tmp_path);
    let mut dst = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&tmp_path)
    {
        Ok(file) => file,
        Err(err) => {
            loge!(
                "failed to open staged temp file {}: {err}",
                log_path(label, &tmp_path)
            );
            return false;
        }
    };

    let mut ok = true;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        match read_fd(src_fd, &mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                if let Err(err) = dst.write_all(&buffer[..size as usize]) {
                    loge!("failed to write {}: {err}", log_path(label, &tmp_path));
                    ok = false;
                    break;
                }
            }
            Err(err) => {
                loge!("failed to read source for {label}: {err}");
                ok = false;
                break;
            }
        }
    }

    if chown_fd(dst.as_raw_fd(), uid, gid).is_err() {
        loge!(
            "failed to chown staged file {}: {}",
            log_path(label, &tmp_path),
            last_errno()
        );
        ok = false;
    }
    if chmod_fd(dst.as_raw_fd(), mode).is_err() {
        loge!(
            "failed to chmod staged file {}: {}",
            log_path(label, &tmp_path),
            last_errno()
        );
        ok = false;
    }
    if ok && fsync_fd(dst.as_raw_fd()).is_err() {
        loge!(
            "failed to fsync staged file {}: {}",
            log_path(label, &tmp_path),
            last_errno()
        );
        ok = false;
    }
    let _ = seek_start(src_fd);
    drop(dst);

    if !ok {
        let _ = fs::remove_file(&tmp_path);
        return false;
    }
    if let Err(err) = fs::rename(&tmp_path, dst_path) {
        loge!(
            "failed to install staged file {}: {err}",
            log_path(label, dst_path)
        );
        let _ = fs::remove_file(&tmp_path);
        return false;
    }
    true
}

fn read_fd(fd: c_int, buffer: &mut [u8]) -> Result<isize, String> {
    loop {
        let read_count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read_count >= 0 {
            return Ok(read_count);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err.to_string());
        }
    }
}

fn chown_path(path: &std::ffi::CStr, uid: JInt, gid: JInt) -> c_int {
    unsafe { libc::chown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) }
}

fn chmod_path(path: &std::ffi::CStr, mode: u32) -> c_int {
    unsafe { libc::chmod(path.as_ptr(), mode as libc::mode_t) }
}

fn chown_fd(fd: c_int, uid: JInt, gid: JInt) -> Result<(), ()> {
    if unsafe { libc::fchown(fd, uid as libc::uid_t, gid as libc::gid_t) } == 0 {
        Ok(())
    } else {
        Err(())
    }
}

fn chmod_fd(fd: c_int, mode: u32) -> Result<(), ()> {
    if unsafe { libc::fchmod(fd, mode as libc::mode_t) } == 0 {
        Ok(())
    } else {
        Err(())
    }
}

fn fsync_fd(fd: c_int) -> Result<(), ()> {
    if unsafe { libc::fsync(fd) } == 0 {
        Ok(())
    } else {
        Err(())
    }
}

fn seek_start(fd: c_int) -> Result<(), ()> {
    if unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } >= 0 {
        Ok(())
    } else {
        Err(())
    }
}

fn open_library_source(library: &PlannedLibrary) -> Result<OwnedFd, String> {
    match &library.location {
        LibraryLocation::ModuleFd(fd) => duplicate_fd(fd.as_raw_fd()),
        LibraryLocation::AbsolutePath(path) | LibraryLocation::StagedPath(path) => {
            open_owned(Path::new(path), libc::O_RDONLY | libc::O_CLOEXEC, 0)
        }
    }
}

fn open_library_sidecar(module_dir_fd: c_int, library: &PlannedLibrary) -> Result<OwnedFd, String> {
    let config_name = sidecar_name_for_library(&library.source_path)
        .ok_or_else(|| format!("invalid library path {}", library.source_path))?;
    if is_absolute_path(&library.source_path) {
        let path = Path::new(dirname(&library.source_path)).join(config_name);
        return open_owned(&path, libc::O_RDONLY | libc::O_CLOEXEC, 0);
    }

    let parent = dirname(&library.source_path);
    let sidecar_path = if parent == "." {
        config_name
    } else {
        format!("{parent}/{config_name}")
    };
    openat_owned(
        module_dir_fd,
        &sidecar_path,
        libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    )
}

fn write_default_gadget_config(dst_path: &Path, label: &str, uid: JInt, gid: JInt) -> bool {
    let tmp_path = temp_path_for(dst_path);
    let _ = fs::remove_file(&tmp_path);
    let mut dst = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&tmp_path)
    {
        Ok(file) => file,
        Err(err) => {
            loge!(
                "failed to open default gadget config {}: {err}",
                log_path(label, &tmp_path)
            );
            return false;
        }
    };
    if let Err(err) = dst.write_all(DEFAULT_GADGET_CONFIG) {
        loge!(
            "failed to write default gadget config {}: {err}",
            log_path(label, &tmp_path)
        );
        drop(dst);
        let _ = fs::remove_file(&tmp_path);
        return false;
    }
    if chown_fd(dst.as_raw_fd(), uid, gid).is_err() {
        loge!(
            "failed to chown default gadget config {}: {}",
            log_path(label, &tmp_path),
            last_errno()
        );
        drop(dst);
        let _ = fs::remove_file(&tmp_path);
        return false;
    }
    if chmod_fd(dst.as_raw_fd(), 0o600).is_err() {
        loge!(
            "failed to chmod default gadget config {}: {}",
            log_path(label, &tmp_path),
            last_errno()
        );
        drop(dst);
        let _ = fs::remove_file(&tmp_path);
        return false;
    }
    if fsync_fd(dst.as_raw_fd()).is_err() {
        loge!(
            "failed to fsync default gadget config {}: {}",
            log_path(label, &tmp_path),
            last_errno()
        );
        drop(dst);
        let _ = fs::remove_file(&tmp_path);
        return false;
    }
    drop(dst);
    if let Err(err) = fs::rename(&tmp_path, dst_path) {
        loge!(
            "failed to install default gadget config {}: {err}",
            log_path(label, dst_path)
        );
        let _ = fs::remove_file(&tmp_path);
        return false;
    }
    true
}

fn temp_path_for(dst_path: &Path) -> PathBuf {
    let file_name = dst_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("staged");
    dst_path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        current_pid(),
        monotonic_temp_id()
    ))
}

fn monotonic_temp_id() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static TEMP_ID: AtomicUsize = AtomicUsize::new(0);
    TEMP_ID.fetch_add(1, Ordering::Relaxed)
}

fn library_display(library: &PlannedLibrary) -> String {
    if crate::logging::verbose_diagnostics() {
        format!("{} ({})", library.label, library.source_path)
    } else {
        library.label.clone()
    }
}

fn log_path(label: &str, path: &Path) -> String {
    if crate::logging::verbose_diagnostics() {
        format!("{label} {}", path.display())
    } else {
        label.to_string()
    }
}

fn staged_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChildGatingConfig, ChildGatingMode, LibraryConfig, StagingMode};
    use crate::execution_plan::build_execution_plan;
    use crate::paths::{current_gid, current_uid, open_owned};
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn write(path: &Path, data: &[u8]) {
        fs::write(path, data).unwrap();
    }

    fn module_fd(dir: &Path) -> OwnedFd {
        open_owned(dir, libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY, 0).unwrap()
    }

    fn staging_config(library: &str) -> crate::config::TargetConfig {
        crate::config::TargetConfig {
            enabled: true,
            app_name: "pkg".to_string(),
            start_up_delay_ms: 0,
            staging: StagingMode::AppData,
            injected_libraries: vec![LibraryConfig::new(library.to_string())],
            child_gating: ChildGatingConfig::default(),
        }
    }

    fn build_staged_plan(
        module_fd: c_int,
        cfg: crate::config::TargetConfig,
        app_data_dir: &str,
    ) -> ExecutionPlan {
        build_execution_plan(
            module_fd,
            cfg,
            Some(app_data_dir),
            current_uid() as JInt,
            current_gid() as JInt,
        )
        .unwrap()
    }

    fn staged_config_path(library_path: &str) -> PathBuf {
        let path = PathBuf::from(library_path);
        let file_name = path.file_name().unwrap().to_string_lossy();
        path.with_file_name(file_name.replace(".so", ".config.so"))
    }

    #[test]
    fn stages_library_and_sidecar() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        write(&module.path().join("libgadget.config.so"), b"config");
        let module_fd = module_fd(module.path());

        let plan = build_staged_plan(
            module_fd.as_raw_fd(),
            staging_config("libgadget.so"),
            app.path().to_str().unwrap(),
        );

        let stage_dir = app.path().join("files/.zygiskfrida");
        let staged_path = PathBuf::from(&plan.libraries[0].load_path);
        let staged_name = staged_path.file_name().unwrap().to_string_lossy();
        assert!(staged_path.starts_with(&stage_dir));
        assert!(is_generated_stage_name(&staged_name));
        assert!(!staged_name.contains("libgadget"));
        assert_eq!(fs::read(&staged_path).unwrap(), b"library");
        assert_eq!(
            fs::read(staged_config_path(&plan.libraries[0].load_path)).unwrap(),
            b"config"
        );
    }

    #[test]
    fn missing_gadget_sidecar_writes_default_config() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());

        let plan = build_staged_plan(
            module_fd.as_raw_fd(),
            staging_config("libgadget.so"),
            app.path().to_str().unwrap(),
        );

        let config = fs::read(staged_config_path(&plan.libraries[0].load_path)).unwrap();
        assert_eq!(config, DEFAULT_GADGET_CONFIG);
    }

    #[test]
    fn missing_app_data_dir_skips_staging() {
        let module = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());
        assert!(
            build_execution_plan(
                module_fd.as_raw_fd(),
                staging_config("libgadget.so"),
                None,
                0,
                0
            )
            .is_none()
        );
    }

    #[test]
    fn stages_child_gating_libraries_with_separate_index_range() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"parent");
        write(&module.path().join("libgadget-child.so"), b"child");
        let module_fd = module_fd(module.path());

        let mut cfg = staging_config("libgadget.so");
        cfg.child_gating = ChildGatingConfig {
            enabled: true,
            mode: ChildGatingMode::Inject,
            injected_libraries: vec![LibraryConfig::new("libgadget-child.so".to_string())],
        };
        let plan = build_staged_plan(module_fd.as_raw_fd(), cfg, app.path().to_str().unwrap());

        let stage_dir = app.path().join("files/.zygiskfrida");
        let child_path = PathBuf::from(&plan.child_gating.libraries[0].load_path);
        let child_name = child_path.file_name().unwrap().to_string_lossy();
        assert!(child_path.starts_with(stage_dir));
        assert!(child_name.starts_with("4000-"));
        assert!(!child_name.contains("libgadget"));
        assert_eq!(fs::read(child_path).unwrap(), b"child");
    }

    #[test]
    fn staged_files_have_private_modes() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());
        let plan = build_staged_plan(
            module_fd.as_raw_fd(),
            staging_config("libgadget.so"),
            app.path().to_str().unwrap(),
        );
        let lib_mode = fs::metadata(&plan.libraries[0].load_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let config_mode = fs::metadata(staged_config_path(&plan.libraries[0].load_path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(lib_mode, 0o700);
        assert_eq!(config_mode, 0o600);
    }

    #[test]
    fn cleans_stale_generated_files_but_keeps_unrelated_files() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let stage_dir = app.path().join("files/.zygiskfrida");
        fs::create_dir_all(&stage_dir).unwrap();
        write(&stage_dir.join("0001-0000000000000000.so"), b"old");
        write(&stage_dir.join("note.txt"), b"keep");
        let module_fd = module_fd(module.path());

        let _plan = build_staged_plan(
            module_fd.as_raw_fd(),
            staging_config("libgadget.so"),
            app.path().to_str().unwrap(),
        );

        assert!(!stage_dir.join("0001-0000000000000000.so").exists());
        assert_eq!(fs::read(stage_dir.join("note.txt")).unwrap(), b"keep");
    }

    #[test]
    fn cleans_old_compat_generated_files_only() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let old_dir = app.path().join("files/zygiskfrida");
        fs::create_dir_all(&old_dir).unwrap();
        write(&old_dir.join("0000-libgadget.so"), b"old");
        write(&old_dir.join("note.txt"), b"keep");
        let module_fd = module_fd(module.path());

        let _plan = build_staged_plan(
            module_fd.as_raw_fd(),
            staging_config("libgadget.so"),
            app.path().to_str().unwrap(),
        );

        assert!(!old_dir.join("0000-libgadget.so").exists());
        assert_eq!(fs::read(old_dir.join("note.txt")).unwrap(), b"keep");
    }

    #[test]
    fn generated_stage_name_detection_is_strict() {
        assert!(is_generated_stage_name("0000-0123456789abcdef.so"));
        assert!(is_generated_stage_name("4000-0123456789abcdef.config.so"));
        assert!(!is_generated_stage_name("0000-libgadget.so"));
        assert!(!is_generated_stage_name("0000-0123456789ABCDEF.so"));
        assert!(!is_generated_stage_name("tmp-0000-libgadget.so"));
        assert!(!is_generated_stage_name("000-libgadget.so"));
        assert!(!is_generated_stage_name("zzzz-libgadget.so"));
    }
}
