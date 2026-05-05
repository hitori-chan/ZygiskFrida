#[macro_use]
mod logging;

#[cfg(target_os = "android")]
mod android_linker;
mod child_gating;
mod config;
mod dynamic_linker;
mod ffi;
mod inject;
mod jni;
mod paths;
mod remap;
mod staging;

use config::{TargetConfig, load_config};
use ffi::{
    ApiTable, AppSpecializeArgs, DLCLOSE_MODULE_LIBRARY, FORCE_DENYLIST_UNMOUNT, JNIEnv, ModuleAbi,
    ServerSpecializeArgs, ZYGISK_API_VERSION,
};

struct RustModule {
    api: *mut ApiTable,
    env: *mut JNIEnv,
    app_name: Option<String>,
    target_config: Option<TargetConfig>,
    keep_loaded_for_hooks: bool,
}

unsafe impl Send for RustModule {}

unsafe extern "C" fn pre_app_specialize(module: *mut RustModule, args: *mut AppSpecializeArgs) {
    if module.is_null() || args.is_null() {
        return;
    }

    let module = unsafe { &mut *module };
    let args = unsafe { &mut *args };
    let Some(app_name) = (unsafe { jni::string(module.env, args.nice_name) }) else {
        return;
    };
    module.app_name = Some(app_name.clone());

    let module_dir_fd = unsafe { (*module.api).get_module_dir() };
    let mut target_config = load_config(module_dir_fd, &app_name);
    if let Some(cfg) = &mut target_config
        && cfg.enabled
        && cfg.stage_libraries_in_app_data
    {
        let Some(app_data_dir) = (unsafe { jni::string(module.env, args.app_data_dir) }) else {
            loge!("cannot stage libraries without app data dir for {app_name}");
            paths::close_fd(module_dir_fd);
            module.target_config = None;
            return;
        };
        let uid = unsafe { args.uid.as_ref().copied().unwrap_or(0) };
        let gid = unsafe { args.gid.as_ref().copied().unwrap_or(0) };
        if !staging::stage_libraries(module_dir_fd, cfg, &app_data_dir, uid, gid) {
            target_config = None;
        }
    }
    paths::close_fd(module_dir_fd);

    if let Some(cfg) = &target_config
        && cfg.enabled
    {
        module.set_option(FORCE_DENYLIST_UNMOUNT);
        if cfg.child_gating.enabled {
            module.keep_loaded_for_hooks = child_gating::install(module.api, &cfg.child_gating);
        }
    }

    module.target_config = target_config;
}

unsafe extern "C" fn post_app_specialize(module: *mut RustModule, _args: *const AppSpecializeArgs) {
    if module.is_null() {
        return;
    }
    let module = unsafe { &mut *module };
    let Some(app_name) = module.app_name.clone() else {
        module.request_dlclose();
        return;
    };
    let Some(cfg) = module.target_config.take() else {
        module.request_dlclose();
        return;
    };

    if !inject::check_and_inject(&app_name, cfg) && !module.keep_loaded_for_hooks {
        module.request_dlclose();
    }
}

unsafe extern "C" fn pre_server_specialize(
    _module: *mut RustModule,
    _args: *mut ServerSpecializeArgs,
) {
}

unsafe extern "C" fn post_server_specialize(
    _module: *mut RustModule,
    _args: *const ServerSpecializeArgs,
) {
}

/// Zygisk module entry point.
///
/// # Safety
///
/// Zygisk calls this with a valid API table and JNI environment pointer. The
/// function mirrors Zygisk's C ABI and leaks the registered module state for
/// the process lifetime, matching Zygisk's registration model.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zygisk_module_entry(table: *mut ApiTable, env: *mut JNIEnv) {
    if table.is_null() {
        return;
    }

    let Some(register_module) = (unsafe { &*table }).register_module else {
        return;
    };

    let module = Box::into_raw(Box::new(RustModule {
        api: table,
        env,
        app_name: None,
        target_config: None,
        keep_loaded_for_hooks: false,
    }));
    let abi = Box::into_raw(Box::new(ModuleAbi {
        api_version: ZYGISK_API_VERSION,
        this: module,
        pre_app_specialize: Some(pre_app_specialize),
        post_app_specialize: Some(post_app_specialize),
        pre_server_specialize: Some(pre_server_specialize),
        post_server_specialize: Some(post_server_specialize),
    }));

    if !(unsafe { register_module(table, abi) }) {
        unsafe {
            drop(Box::from_raw(abi));
            drop(Box::from_raw(module));
        }
    }
}

/// Optional companion entry kept for ABI completeness; this module does not
/// need a root companion process.
#[unsafe(no_mangle)]
pub extern "C" fn zygisk_companion_entry(_client: i32) {}

impl RustModule {
    fn request_dlclose(&self) {
        if !self.keep_loaded_for_hooks {
            self.set_option(DLCLOSE_MODULE_LIBRARY);
        }
    }

    fn set_option(&self, option: i32) {
        unsafe {
            (*self.api).set_option(option);
        }
    }
}
