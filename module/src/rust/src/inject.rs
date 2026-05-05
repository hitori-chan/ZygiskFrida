use std::fs::File;
use std::io::Read;
use std::thread;
use std::time::Duration;

use crate::config::{LibraryConfig, TargetConfig};
use crate::dynamic_linker::{self, LoadStatus};
use crate::paths::c_string;
use crate::remap;

pub(crate) fn check_and_inject(app_name: &str, cfg: TargetConfig) -> bool {
    logi!("App detected: {app_name}");
    logi!("PID: {}", unsafe { libc::getpid() });

    if !cfg.enabled {
        logi!("Injection disabled for {app_name}");
        return false;
    }

    if let Err(err) = thread::Builder::new().spawn(move || inject_libraries(cfg)) {
        loge!("failed to start injection thread: {err}");
        return false;
    }
    true
}

pub(crate) fn inject_library(library: &LibraryConfig, log_context: &str) {
    inject_lib_with_remap_hints(
        &library.path,
        [library.path.as_str(), library.source_path.as_str()],
        log_context,
    );
}

fn inject_lib_with_remap_hints<'a>(
    lib_path: &str,
    remap_hints: impl IntoIterator<Item = &'a str>,
    log_context: &str,
) {
    let Ok(path) = c_string(lib_path) else {
        loge!("{log_context}Failed to inject {lib_path}: path contains NUL byte");
        return;
    };
    let remap_hints: Vec<_> = remap_hints.into_iter().collect();

    match dynamic_linker::load(&path, lib_path) {
        Ok(LoadStatus::Opened { handle, method }) => {
            logi!("{log_context}Injected {lib_path} with handle {handle:p} ({method})");
            remap_loaded_library(&remap_hints);
        }
        Ok(LoadStatus::AlreadyMapped) => {
            logi!("{log_context}Library {lib_path} is already mapped");
            remap_loaded_library(&remap_hints);
        }
        Err(errors) => {
            for error in errors {
                loge!(
                    "{log_context}Failed to inject {lib_path} ({}): {}",
                    error.method,
                    error.error
                );
            }
        }
    }
}

fn remap_loaded_library(paths: &[&str]) {
    for path in paths {
        if remap::remap_lib(path) {
            return;
        }
    }
}

fn inject_libraries(cfg: TargetConfig) {
    wait_for_init(&cfg.app_name);
    delay_start_up(cfg.start_up_delay_ms);

    for library in &cfg.injected_libraries {
        logi!("Injecting {}", library.path);
        inject_library(library, "");
    }
}

fn current_process_name() -> Option<String> {
    let mut bytes = Vec::new();
    if File::open("/proc/self/cmdline")
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .is_err()
    {
        return None;
    }
    process_name_from_cmdline(&bytes)
}

fn wait_for_init(app_name: &str) {
    logi!("Wait for process to complete init");
    while current_process_name().as_deref() != Some(app_name) {
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(100));
    logi!("Process init completed");
}

fn process_name_from_cmdline(cmdline: &[u8]) -> Option<String> {
    let end = cmdline
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(cmdline.len());
    if end == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&cmdline[..end]).into_owned())
}

fn delay_start_up(start_up_delay_ms: u64) {
    if start_up_delay_ms == 0 {
        return;
    }
    logi!("Waiting for configured start up delay {start_up_delay_ms}ms");

    let mut countdown = 0;
    let mut delay = start_up_delay_ms;
    for _ in 0..10 {
        if delay <= 1000 {
            break;
        }
        delay -= 1000;
        countdown += 1;
    }

    thread::sleep(Duration::from_millis(delay));
    for remaining in (1..=countdown).rev() {
        logi!("Injecting libs in {remaining} seconds");
        thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_first_cmdline_argument_as_process_name() {
        assert_eq!(
            process_name_from_cmdline(b"com.example.app\0--zygote\0").as_deref(),
            Some("com.example.app")
        );
    }

    #[test]
    fn empty_cmdline_has_no_process_name() {
        assert_eq!(process_name_from_cmdline(b"\0"), None);
        assert_eq!(process_name_from_cmdline(b""), None);
    }

    #[test]
    fn process_detection_is_exact_not_substring() {
        assert_ne!(
            process_name_from_cmdline(b"com.example.app2\0").as_deref(),
            Some("com.example.app")
        );
    }
}
