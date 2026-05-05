#[macro_use]
mod logging;

#[cfg(target_os = "android")]
mod android_linker;
mod child_gating;
mod config;
mod dynamic_linker;
mod execution_plan;
mod ffi;
mod inject;
mod jni;
mod paths;
mod remap;
mod staging;

use execution_plan::{ExecutionPlan, build_execution_plan};
use ffi::{
    ApiTable, AppSpecializeArgs, DLCLOSE_MODULE_LIBRARY, FORCE_DENYLIST_UNMOUNT, JNIEnv, ModuleAbi,
    ServerSpecializeArgs, ZYGISK_API_VERSION,
};

struct RustModule {
    api: *mut ApiTable,
    env: *mut JNIEnv,
    app_name: Option<String>,
    execution_plan: Option<ExecutionPlan>,
    keep_loaded_for_hooks: bool,
}

unsafe impl Send for RustModule {}

unsafe extern "C" fn pre_app_specialize(module: *mut RustModule, args: *mut AppSpecializeArgs) {
    let Some(module) = rust_module_from_ptr(module) else {
        return;
    };
    let Some(args) = app_specialize_args_from_ptr(args) else {
        return;
    };
    let Some(app_name) = jni_string(module.env, args.nice_name) else {
        return;
    };
    module.app_name = Some(app_name.clone());

    let module_dir_fd = module.module_dir_fd();
    let target_config = config::load_config(module_dir_fd, &app_name);
    let app_data_dir = jni_string(module.env, args.app_data_dir);
    let uid = optional_jint(args.uid);
    let gid = optional_jint(args.gid);
    let execution_plan = target_config.and_then(|cfg| {
        build_execution_plan(module_dir_fd, cfg, app_data_dir.as_deref(), uid, gid)
    });
    paths::close_fd(module_dir_fd);

    if let Some(plan) = &execution_plan
        && plan.enabled
    {
        module.set_option(FORCE_DENYLIST_UNMOUNT);
        if plan.child_gating.enabled {
            module.keep_loaded_for_hooks = child_gating::install(module.api, &plan.child_gating);
        }
    }

    module.execution_plan = execution_plan;
}

unsafe extern "C" fn post_app_specialize(module: *mut RustModule, _args: *const AppSpecializeArgs) {
    let Some(module) = rust_module_from_ptr(module) else {
        return;
    };
    let Some(app_name) = module.app_name.clone() else {
        module.request_dlclose();
        return;
    };
    let Some(plan) = module.execution_plan.take() else {
        module.request_dlclose();
        return;
    };

    if plan.app_name != app_name {
        loge!(
            "execution plan app mismatch: {} != {app_name}",
            plan.app_name
        );
        module.request_dlclose();
        return;
    }

    if !inject::check_and_inject(plan) && !module.keep_loaded_for_hooks {
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
    let Some(api) = api_from_ptr(table) else {
        return;
    };
    let Some(register_module) = api.register_module else {
        return;
    };

    let module = Box::into_raw(Box::new(RustModule {
        api: table,
        env,
        app_name: None,
        execution_plan: None,
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

    if !register_zygisk_module(register_module, table, abi) {
        drop_raw_module_registration(abi, module);
    }
}

/// Optional companion entry kept for ABI completeness; this module does not
/// need a root companion process.
#[unsafe(no_mangle)]
pub extern "C" fn zygisk_companion_entry(_client: i32) {}

impl RustModule {
    fn module_dir_fd(&self) -> libc::c_int {
        unsafe { (*self.api).get_module_dir() }
    }

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

fn rust_module_from_ptr<'a>(module: *mut RustModule) -> Option<&'a mut RustModule> {
    if module.is_null() {
        None
    } else {
        Some(unsafe { &mut *module })
    }
}

fn app_specialize_args_from_ptr<'a>(
    args: *mut AppSpecializeArgs,
) -> Option<&'a mut AppSpecializeArgs> {
    if args.is_null() {
        None
    } else {
        Some(unsafe { &mut *args })
    }
}

fn api_from_ptr<'a>(api: *mut ApiTable) -> Option<&'a ApiTable> {
    if api.is_null() {
        None
    } else {
        Some(unsafe { &*api })
    }
}

fn jni_string(env: *mut JNIEnv, value: *mut ffi::JString) -> Option<String> {
    unsafe { jni::string(env, value) }
}

fn optional_jint(value: *mut ffi::JInt) -> ffi::JInt {
    unsafe { value.as_ref().copied().unwrap_or(0) }
}

fn register_zygisk_module(
    register_module: unsafe extern "C" fn(*mut ApiTable, *mut ModuleAbi) -> bool,
    table: *mut ApiTable,
    abi: *mut ModuleAbi,
) -> bool {
    unsafe { register_module(table, abi) }
}

fn drop_raw_module_registration(abi: *mut ModuleAbi, module: *mut RustModule) {
    unsafe {
        drop(Box::from_raw(abi));
        drop(Box::from_raw(module));
    }
}
