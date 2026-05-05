use std::fs::File;
use std::io::Read;
use std::thread;
use std::time::Duration;

use crate::dynamic_linker::{self, LoadStatus};
use crate::execution_plan::{ExecutionPlan, PlannedLibrary};
use crate::paths::{c_string, current_pid};
use crate::remap;

pub(crate) fn check_and_inject(plan: ExecutionPlan) -> bool {
    let app_name = plan.app_name.clone();
    logi!("App detected: {app_name}");
    logi!("PID: {}", current_pid());

    if !plan.enabled {
        logi!("Injection disabled for {app_name}");
        return false;
    }

    if let Err(err) = thread::Builder::new().spawn(move || inject_libraries(plan)) {
        loge!("failed to start injection thread: {err}");
        return false;
    }
    true
}

pub(crate) fn inject_library(library: &PlannedLibrary, log_context: &str) {
    inject_library_by_path(
        &library.load_path,
        &library.remap_hints,
        &library.label,
        log_context,
    );
}

pub(crate) fn inject_library_by_path(
    lib_path: &str,
    remap_hints: &[String],
    label: &str,
    log_context: &str,
) {
    let Ok(path) = c_string(lib_path) else {
        loge!("{log_context}Failed to inject {label}: path contains NUL byte");
        return;
    };

    match dynamic_linker::load(&path, lib_path) {
        Ok(LoadStatus::Opened { handle, method }) => {
            logi!(
                "{log_context}Injected {} with handle {handle:p} ({method})",
                library_for_log(label, lib_path)
            );
            remap_loaded_library(remap_hints, label);
        }
        Ok(LoadStatus::AlreadyMapped) => {
            logi!(
                "{log_context}Library {} is already mapped",
                library_for_log(label, lib_path)
            );
            remap_loaded_library(remap_hints, label);
        }
        Err(errors) => {
            for error in errors {
                let error_message =
                    loader_error_for_log(&error.error, lib_path, remap_hints, label);
                loge!(
                    "{log_context}Failed to inject {} ({}): {}",
                    library_for_log(label, lib_path),
                    error.method,
                    error_message
                );
            }
        }
    }
}

fn remap_loaded_library(paths: &[String], label: &str) {
    if !remap::remap_lib(paths, label) {
        if crate::logging::verbose_diagnostics() {
            let hints = format!("{paths:?}");
            loge!("failed to remap loaded library {label} for hints {hints}");
        } else {
            loge!("failed to remap loaded library {label}");
        }
    }
}

fn inject_libraries(plan: ExecutionPlan) {
    wait_for_init(&plan.app_name);
    delay_start_up(plan.start_up_delay_ms);

    for library in &plan.libraries {
        logi!(
            "Injecting {}",
            library_for_log(&library.label, &library.load_path)
        );
        inject_library(library, "");
    }
}

fn library_for_log(label: &str, path: &str) -> String {
    if crate::logging::verbose_diagnostics() {
        format!("{label} ({path})")
    } else {
        label.to_string()
    }
}

fn loader_error_for_log(
    error: &str,
    lib_path: &str,
    remap_hints: &[String],
    label: &str,
) -> String {
    loader_error_for_log_mode(
        error,
        lib_path,
        remap_hints,
        label,
        crate::logging::verbose_diagnostics(),
    )
}

fn loader_error_for_log_mode(
    error: &str,
    lib_path: &str,
    remap_hints: &[String],
    label: &str,
    verbose: bool,
) -> String {
    if verbose {
        return error.to_string();
    }

    let mut redacted = error.replace(lib_path, label);
    for hint in remap_hints {
        if !hint.is_empty() {
            redacted = redacted.replace(hint, label);
        }
    }
    redact_proc_fd_paths(&redacted, label)
}

fn redact_proc_fd_paths(message: &str, label: &str) -> String {
    let mut output = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(index) = rest.find("/proc/self/fd/") {
        output.push_str(&rest[..index]);
        output.push_str(label);
        let after_prefix = &rest[index + "/proc/self/fd/".len()..];
        let end = after_prefix
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(after_prefix.len());
        rest = &after_prefix[end..];
    }
    output.push_str(rest);
    output
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

    #[test]
    fn release_loader_error_redacts_paths() {
        let message = loader_error_for_log(
            "dlopen failed: /proc/self/fd/12 and /data/app/libgadget.so missing",
            "/proc/self/fd/12",
            &["/data/app/libgadget.so".to_string()],
            "parent[0]",
        );

        assert_eq!(
            message.contains("/proc/self/fd/12"),
            crate::logging::verbose_diagnostics()
        );
    }

    #[test]
    fn release_loader_error_formatter_uses_label_not_paths() {
        let message = loader_error_for_log_mode(
            "dlopen failed: /proc/self/fd/12 and /data/app/libgadget.so missing",
            "/proc/self/fd/12",
            &["/data/app/libgadget.so".to_string()],
            "parent[0]",
            false,
        );

        assert_eq!(message, "dlopen failed: parent[0] and parent[0] missing");
    }

    #[test]
    fn verbose_loader_error_formatter_keeps_paths() {
        let message = loader_error_for_log_mode(
            "dlopen failed: /proc/self/fd/12 missing",
            "/proc/self/fd/12",
            &[],
            "parent[0]",
            true,
        );

        assert!(message.contains("/proc/self/fd/12"));
    }
}
