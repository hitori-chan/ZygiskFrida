use std::ffi::{c_char, c_int, c_void};

use crate::RustModule;

pub(crate) const ZYGISK_API_VERSION: i64 = 2;
pub(crate) const FORCE_DENYLIST_UNMOUNT: c_int = 0;
pub(crate) const DLCLOSE_MODULE_LIBRARY: c_int = 1;

pub(crate) type JInt = i32;
pub(crate) type JLong = i64;
pub(crate) type JBoolean = u8;
pub(crate) type JObject = *mut c_void;
pub(crate) type JString = JObject;
pub(crate) type JIntArray = JObject;
pub(crate) type JObjectArray = JObject;

#[repr(C)]
pub struct JNIEnv {
    pub(crate) functions: *const c_void,
}

#[repr(C)]
#[allow(dead_code)]
pub(crate) struct JNINativeMethod {
    name: *const c_char,
    signature: *const c_char,
    fn_ptr: *mut c_void,
}

#[repr(C)]
#[allow(dead_code)]
pub struct ApiTable {
    pub(crate) this: *mut c_void,
    pub(crate) register_module: Option<unsafe extern "C" fn(*mut ApiTable, *mut ModuleAbi) -> bool>,
    hook_jni_native_methods:
        Option<unsafe extern "C" fn(*mut JNIEnv, *const c_char, *mut JNINativeMethod, c_int)>,
    pub(crate) plt_hook_register:
        Option<unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void, *mut *mut c_void)>,
    plt_hook_exclude: Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    pub(crate) plt_hook_commit: Option<unsafe extern "C" fn() -> bool>,
    connect_companion: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    set_option: Option<unsafe extern "C" fn(*mut c_void, c_int)>,
    get_module_dir: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    get_flags: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
}

impl ApiTable {
    pub(crate) unsafe fn get_module_dir(&self) -> c_int {
        let Some(get_module_dir) = self.get_module_dir else {
            return -1;
        };
        unsafe { get_module_dir(self.this) }
    }

    pub(crate) unsafe fn set_option(&self, option: c_int) {
        if let Some(set_option) = self.set_option {
            unsafe {
                set_option(self.this, option);
            }
        }
    }
}

#[repr(C)]
pub(crate) struct ModuleAbi {
    pub(crate) api_version: i64,
    pub(crate) this: *mut RustModule,
    pub(crate) pre_app_specialize:
        Option<unsafe extern "C" fn(*mut RustModule, *mut AppSpecializeArgs)>,
    pub(crate) post_app_specialize:
        Option<unsafe extern "C" fn(*mut RustModule, *const AppSpecializeArgs)>,
    pub(crate) pre_server_specialize:
        Option<unsafe extern "C" fn(*mut RustModule, *mut ServerSpecializeArgs)>,
    pub(crate) post_server_specialize:
        Option<unsafe extern "C" fn(*mut RustModule, *const ServerSpecializeArgs)>,
}

#[repr(C)]
#[allow(dead_code)]
pub(crate) struct AppSpecializeArgs {
    pub(crate) uid: *mut JInt,
    pub(crate) gid: *mut JInt,
    gids: *mut JIntArray,
    runtime_flags: *mut JInt,
    mount_external: *mut JInt,
    se_info: *mut JString,
    pub(crate) nice_name: *mut JString,
    instruction_set: *mut JString,
    pub(crate) app_data_dir: *mut JString,
    is_child_zygote: *mut JBoolean,
    is_top_app: *mut JBoolean,
    pkg_data_info_list: *mut JObjectArray,
    whitelisted_data_info_list: *mut JObjectArray,
    mount_data_dirs: *mut JBoolean,
    mount_storage_dirs: *mut JBoolean,
}

#[repr(C)]
#[allow(dead_code)]
pub(crate) struct ServerSpecializeArgs {
    uid: *mut JInt,
    gid: *mut JInt,
    gids: *mut JIntArray,
    runtime_flags: *mut JInt,
    permitted_capabilities: *mut JLong,
    effective_capabilities: *mut JLong,
}
