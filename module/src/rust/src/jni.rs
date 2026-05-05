use std::ffi::{CStr, c_char, c_void};
use std::mem;
use std::ptr;

use crate::ffi::{JBoolean, JNIEnv, JString};

const JNI_GET_STRING_UTF_CHARS: isize = 169;
const JNI_RELEASE_STRING_UTF_CHARS: isize = 170;

pub(crate) unsafe fn string(env: *mut JNIEnv, value: *mut JString) -> Option<String> {
    if env.is_null() || value.is_null() {
        return None;
    }
    let string = unsafe { *value };
    if string.is_null() {
        return None;
    }

    let table = unsafe { (*env).functions.cast::<*const c_void>() };
    if table.is_null() {
        return None;
    }

    type GetStringUtfChars =
        unsafe extern "C" fn(*mut JNIEnv, JString, *mut JBoolean) -> *const c_char;
    type ReleaseStringUtfChars = unsafe extern "C" fn(*mut JNIEnv, JString, *const c_char);

    let get_ptr = unsafe { *table.offset(JNI_GET_STRING_UTF_CHARS) };
    let release_ptr = unsafe { *table.offset(JNI_RELEASE_STRING_UTF_CHARS) };
    if get_ptr.is_null() || release_ptr.is_null() {
        return None;
    }

    let get_string: GetStringUtfChars = unsafe { mem::transmute(get_ptr) };
    let release_string: ReleaseStringUtfChars = unsafe { mem::transmute(release_ptr) };
    let raw = unsafe { get_string(env, string, ptr::null_mut()) };
    if raw.is_null() {
        return None;
    }
    let result = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    unsafe {
        release_string(env, string, raw);
    }
    Some(result)
}
