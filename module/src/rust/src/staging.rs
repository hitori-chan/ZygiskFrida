use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use libc::c_int;

use crate::config::{LibraryConfig, TargetConfig};
use crate::ffi::JInt;
use crate::paths::{
    basename, build_stage_names, c_string, dirname, is_absolute_path, last_errno, open_owned,
    openat_owned, sidecar_name_for_library,
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

pub(crate) fn stage_libraries(
    module_dir_fd: c_int,
    cfg: &mut TargetConfig,
    app_data_dir: &str,
    uid: JInt,
    gid: JInt,
) -> bool {
    if !cfg.stage_libraries_in_app_data {
        return true;
    }
    if module_dir_fd < 0 || app_data_dir.is_empty() {
        loge!("cannot stage libraries without module dir fd and app data dir");
        return false;
    }

    let files_dir = Path::new(app_data_dir).join("files");
    let stage_dir = files_dir.join("zygiskfrida");
    if !ensure_app_files_dir(&files_dir, 0o700, uid, gid)
        || !ensure_owned_dir(&stage_dir, 0o700, uid, gid)
    {
        return false;
    }
    if !stage_library_list(
        module_dir_fd,
        &mut cfg.injected_libraries,
        &stage_dir,
        0,
        uid,
        gid,
    ) {
        return false;
    }
    if cfg.child_gating.enabled
        && !stage_library_list(
            module_dir_fd,
            &mut cfg.child_gating.injected_libraries,
            &stage_dir,
            0x4000,
            uid,
            gid,
        )
    {
        return false;
    }
    true
}

fn stage_library_list(
    module_dir_fd: c_int,
    libraries: &mut [LibraryConfig],
    stage_dir: &Path,
    index_offset: usize,
    uid: JInt,
    gid: JInt,
) -> bool {
    for (index, library) in libraries.iter_mut().enumerate() {
        let Some((library_name, config_name)) =
            build_stage_names(&library.source_path, index_offset + index)
        else {
            loge!("failed to build staged names for {}", library.source_path);
            return false;
        };

        let staged_path = stage_dir.join(library_name);
        let src_fd = match open_library_source(library) {
            Ok(fd) => fd,
            Err(err) => {
                loge!(
                    "failed to open source library {} for staging: {err}",
                    library.source_path
                );
                return false;
            }
        };
        if !copy_fd_to_path(src_fd.as_raw_fd(), &staged_path, uid, gid, 0o700) {
            return false;
        }

        let staged_config_path = stage_dir.join(config_name);
        let _ = fs::remove_file(&staged_config_path);
        match open_library_sidecar(module_dir_fd, library) {
            Ok(sidecar_fd) => {
                if !copy_fd_to_path(sidecar_fd.as_raw_fd(), &staged_config_path, uid, gid, 0o600) {
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
                if !write_default_gadget_config(&staged_config_path, uid, gid) {
                    return false;
                }
            }
            Err(_) => {}
        }

        library.path = staged_path.to_string_lossy().into_owned();
        library.fd = None;
    }
    true
}

fn ensure_app_files_dir(path: &Path, mode: u32, uid: JInt, gid: JInt) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                loge!("{} exists but is not a directory", path.display());
                return false;
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Err(err) = fs::create_dir(path) {
                loge!("failed to create {}: {err}", path.display());
                return false;
            }
        }
        Err(err) => {
            loge!("failed to stat {}: {err}", path.display());
            return false;
        }
    }
    chown_chmod(path, mode, uid, gid)
}

fn ensure_owned_dir(path: &Path, mode: u32, uid: JInt, gid: JInt) -> bool {
    if let Err(err) = fs::create_dir(path)
        && err.kind() != std::io::ErrorKind::AlreadyExists
    {
        loge!("failed to create {}: {err}", path.display());
        return false;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            loge!("{} exists but is not a directory", path.display());
            return false;
        }
        Err(err) => {
            loge!("failed to stat {}: {err}", path.display());
            return false;
        }
    }
    chown_chmod(path, mode, uid, gid)
}

fn chown_chmod(path: &Path, mode: u32, uid: JInt, gid: JInt) -> bool {
    let Ok(path) = c_string(path.to_string_lossy().as_ref()) else {
        loge!("path contains NUL byte");
        return false;
    };
    if unsafe { libc::chown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) } != 0 {
        loge!("failed to chown path: {}", last_errno());
        return false;
    }
    if unsafe { libc::chmod(path.as_ptr(), mode as libc::mode_t) } != 0 {
        loge!("failed to chmod path: {}", last_errno());
        return false;
    }
    true
}

fn copy_fd_to_path(src_fd: c_int, dst_path: &Path, uid: JInt, gid: JInt, mode: u32) -> bool {
    unsafe {
        if libc::lseek(src_fd, 0, libc::SEEK_SET) < 0 {
            loge!(
                "failed to seek source for {}: {}",
                dst_path.display(),
                last_errno()
            );
            return false;
        }
    }

    let _ = fs::remove_file(dst_path);
    let mut dst = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(dst_path)
    {
        Ok(file) => file,
        Err(err) => {
            loge!("failed to open staged file {}: {err}", dst_path.display());
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
                    loge!("failed to write {}: {err}", dst_path.display());
                    ok = false;
                    break;
                }
            }
            Err(err) => {
                loge!("failed to read source for {}: {}", dst_path.display(), err);
                ok = false;
                break;
            }
        }
    }

    unsafe {
        if libc::fchown(dst.as_raw_fd(), uid as libc::uid_t, gid as libc::gid_t) != 0 {
            loge!(
                "failed to chown staged file {}: {}",
                dst_path.display(),
                last_errno()
            );
            ok = false;
        }
        if libc::fchmod(dst.as_raw_fd(), mode as libc::mode_t) != 0 {
            loge!(
                "failed to chmod staged file {}: {}",
                dst_path.display(),
                last_errno()
            );
            ok = false;
        }
        let _ = libc::lseek(src_fd, 0, libc::SEEK_SET);
    }
    drop(dst);
    if !ok {
        let _ = fs::remove_file(dst_path);
    }
    ok
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

fn open_library_source(library: &LibraryConfig) -> Result<OwnedFd, String> {
    if let Some(fd) = &library.fd {
        let dup_fd = unsafe { libc::dup(fd.as_raw_fd()) };
        if dup_fd < 0 {
            return Err(last_errno());
        }
        return Ok(unsafe { OwnedFd::from_raw_fd(dup_fd) });
    }
    open_owned(
        Path::new(&library.source_path),
        libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    )
}

fn open_library_sidecar(module_dir_fd: c_int, library: &LibraryConfig) -> Result<OwnedFd, String> {
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

fn write_default_gadget_config(dst_path: &Path, uid: JInt, gid: JInt) -> bool {
    let _ = fs::remove_file(dst_path);
    let mut dst = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(dst_path)
    {
        Ok(file) => file,
        Err(err) => {
            loge!(
                "failed to open default gadget config {}: {err}",
                dst_path.display()
            );
            return false;
        }
    };
    if let Err(err) = dst.write_all(DEFAULT_GADGET_CONFIG) {
        loge!(
            "failed to write default gadget config {}: {err}",
            dst_path.display()
        );
        let _ = fs::remove_file(dst_path);
        return false;
    }
    unsafe {
        if libc::fchown(dst.as_raw_fd(), uid as libc::uid_t, gid as libc::gid_t) != 0 {
            loge!(
                "failed to chown default gadget config {}: {}",
                dst_path.display(),
                last_errno()
            );
            drop(dst);
            let _ = fs::remove_file(dst_path);
            return false;
        }
        if libc::fchmod(dst.as_raw_fd(), 0o600) != 0 {
            loge!(
                "failed to chmod default gadget config {}: {}",
                dst_path.display(),
                last_errno()
            );
            drop(dst);
            let _ = fs::remove_file(dst_path);
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_library_paths;
    use crate::config::{ChildGatingConfig, ChildGatingMode, LibraryConfig, TargetConfig};
    use crate::paths::open_owned;
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn write(path: &Path, data: &[u8]) {
        fs::write(path, data).unwrap();
    }

    fn module_fd(dir: &Path) -> OwnedFd {
        open_owned(dir, libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY, 0).unwrap()
    }

    fn staging_config(library: &str) -> TargetConfig {
        TargetConfig {
            enabled: true,
            app_name: "pkg".to_string(),
            start_up_delay_ms: 0,
            stage_libraries_in_app_data: true,
            injected_libraries: vec![LibraryConfig::new(library.to_string())],
            child_gating: ChildGatingConfig::default(),
        }
    }

    #[test]
    fn stages_library_and_sidecar() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        write(&module.path().join("libgadget.config.so"), b"config");
        let module_fd = module_fd(module.path());

        let mut cfg = staging_config("libgadget.so");
        assert!(resolve_library_paths(module_fd.as_raw_fd(), &mut cfg));
        assert!(stage_libraries(
            module_fd.as_raw_fd(),
            &mut cfg,
            app.path().to_str().unwrap(),
            unsafe { libc::getuid() as JInt },
            unsafe { libc::getgid() as JInt },
        ));

        let stage_dir = app.path().join("files/zygiskfrida");
        assert_eq!(
            fs::read(stage_dir.join("0000-libgadget.so")).unwrap(),
            b"library"
        );
        assert_eq!(
            fs::read(stage_dir.join("0000-libgadget.config.so")).unwrap(),
            b"config"
        );
        assert_eq!(
            cfg.injected_libraries[0].path,
            stage_dir.join("0000-libgadget.so").to_string_lossy()
        );
    }

    #[test]
    fn missing_gadget_sidecar_writes_default_config() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());

        let mut cfg = staging_config("libgadget.so");
        assert!(resolve_library_paths(module_fd.as_raw_fd(), &mut cfg));
        assert!(stage_libraries(
            module_fd.as_raw_fd(),
            &mut cfg,
            app.path().to_str().unwrap(),
            unsafe { libc::getuid() as JInt },
            unsafe { libc::getgid() as JInt },
        ));

        let config = fs::read(
            app.path()
                .join("files/zygiskfrida/0000-libgadget.config.so"),
        )
        .unwrap();
        assert_eq!(config, DEFAULT_GADGET_CONFIG);
    }

    #[test]
    fn missing_app_data_dir_skips_staging() {
        let module = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());
        let mut cfg = staging_config("libgadget.so");
        assert!(resolve_library_paths(module_fd.as_raw_fd(), &mut cfg));
        assert!(!stage_libraries(module_fd.as_raw_fd(), &mut cfg, "", 0, 0));
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
        assert!(resolve_library_paths(module_fd.as_raw_fd(), &mut cfg));
        assert!(stage_libraries(
            module_fd.as_raw_fd(),
            &mut cfg,
            app.path().to_str().unwrap(),
            unsafe { libc::getuid() as JInt },
            unsafe { libc::getgid() as JInt },
        ));

        let stage_dir = app.path().join("files/zygiskfrida");
        assert_eq!(
            fs::read(stage_dir.join("4000-libgadget-child.so")).unwrap(),
            b"child"
        );
    }

    #[test]
    fn staged_files_have_private_modes() {
        let module = tempdir().unwrap();
        let app = tempdir().unwrap();
        write(&module.path().join("libgadget.so"), b"library");
        let module_fd = module_fd(module.path());
        let mut cfg = staging_config("libgadget.so");
        assert!(resolve_library_paths(module_fd.as_raw_fd(), &mut cfg));
        assert!(stage_libraries(
            module_fd.as_raw_fd(),
            &mut cfg,
            app.path().to_str().unwrap(),
            unsafe { libc::getuid() as JInt },
            unsafe { libc::getgid() as JInt },
        ));
        let lib_mode = fs::metadata(app.path().join("files/zygiskfrida/0000-libgadget.so"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let config_mode = fs::metadata(
            app.path()
                .join("files/zygiskfrida/0000-libgadget.config.so"),
        )
        .unwrap()
        .permissions()
        .mode()
            & 0o777;
        assert_eq!(lib_mode, 0o700);
        assert_eq!(config_mode, 0o600);
    }
}
