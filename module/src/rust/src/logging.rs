use std::ffi::c_int;
#[cfg(target_os = "android")]
use std::ffi::{CString, c_char};

#[cfg(all(target_os = "android", any(debug_assertions, feature = "verbose-logs")))]
pub(crate) const ANDROID_LOG_INFO: c_int = 4;
#[cfg(target_os = "android")]
pub(crate) const ANDROID_LOG_ERROR: c_int = 6;

#[cfg(all(
    not(target_os = "android"),
    any(debug_assertions, feature = "verbose-logs")
))]
pub(crate) const ANDROID_LOG_INFO: c_int = 4;
#[cfg(not(target_os = "android"))]
pub(crate) const ANDROID_LOG_ERROR: c_int = 6;

#[cfg(target_os = "android")]
#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_print(prio: c_int, tag: *const c_char, fmt: *const c_char, ...) -> c_int;
}

pub(crate) fn log_message(priority: c_int, message: String) {
    #[cfg(target_os = "android")]
    {
        let sanitized = message.replace('\0', "\\0");
        if let Ok(msg) = CString::new(sanitized) {
            unsafe {
                __android_log_print(
                    priority,
                    c"ZygiskFrida".as_ptr(),
                    c"%s".as_ptr(),
                    msg.as_ptr(),
                );
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = priority;
        eprintln!("ZygiskFrida: {message}");
    }
}

pub(crate) fn verbose_diagnostics() -> bool {
    cfg!(any(debug_assertions, feature = "verbose-logs"))
}

#[macro_export]
macro_rules! logi {
    ($($arg:tt)*) => {
        #[cfg(any(debug_assertions, feature = "verbose-logs"))]
        {
            $crate::logging::log_message($crate::logging::ANDROID_LOG_INFO, format!($($arg)*))
        }
        #[cfg(not(any(debug_assertions, feature = "verbose-logs")))]
        {
            let _ = format_args!($($arg)*);
        }
    };
}

#[macro_export]
macro_rules! loge {
    ($($arg:tt)*) => {
        $crate::logging::log_message($crate::logging::ANDROID_LOG_ERROR, format!($($arg)*))
    };
}
