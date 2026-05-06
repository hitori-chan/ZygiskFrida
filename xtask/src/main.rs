use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use zip::write::FileOptions;

const MODULE_ID: &str = "zygiskfrida";
const MODULE_NAME: &str = "ZygiskFrida";
const MODULE_AUTHOR: &str = "lico-n, hitori-chan";
const MODULE_DESCRIPTION: &str = "Injects Frida Gadget via Zygisk.";
const MODULE_VERSION_CODE: u32 = 15;
const MODULE_UPDATE_JSON: &str =
    "https://raw.githubusercontent.com/hitori-chan/ZygiskFrida/main/update.json";
const FRIDA_VERSION: &str = "17.9.6";
const TARGET_SDK: u32 = 32;
const NDK_VERSION: &str = "25.2.9519653";

const ANDROID_TARGETS: &[AndroidTarget] = &[
    AndroidTarget {
        rust_target: "armv7-linux-androideabi",
        linker_env: "CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER",
        linker_prefix: "armv7a-linux-androideabi",
        module_lib_name: "armeabi-v7a.so",
        frida_download_arch: "arm",
        frida_module_arch: "arm",
    },
    AndroidTarget {
        rust_target: "aarch64-linux-android",
        linker_env: "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER",
        linker_prefix: "aarch64-linux-android",
        module_lib_name: "arm64-v8a.so",
        frida_download_arch: "arm64",
        frida_module_arch: "arm64",
    },
    AndroidTarget {
        rust_target: "i686-linux-android",
        linker_env: "CARGO_TARGET_I686_LINUX_ANDROID_LINKER",
        linker_prefix: "i686-linux-android",
        module_lib_name: "x86.so",
        frida_download_arch: "x86",
        frida_module_arch: "x86",
    },
    AndroidTarget {
        rust_target: "x86_64-linux-android",
        linker_env: "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER",
        linker_prefix: "x86_64-linux-android",
        module_lib_name: "x86_64.so",
        frida_download_arch: "x86_64",
        frida_module_arch: "x64",
    },
];

#[derive(Clone, Copy)]
struct AndroidTarget {
    rust_target: &'static str,
    linker_env: &'static str,
    linker_prefix: &'static str,
    module_lib_name: &'static str,
    frida_download_arch: &'static str,
    frida_module_arch: &'static str,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("package") => package(),
        Some("build") => build_rust(),
        Some("download-gadget") => download_gadgets().map(|_| ()),
        Some("flash") => {
            package()?;
            flash(false)
        }
        Some("flash-and-reboot") => {
            package()?;
            flash(true)
        }
        Some("-h" | "--help") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(format!("unknown xtask command {other:?}")),
    }
}

fn print_help() {
    println!(
        "\
Usage: cargo xtask <command>

Commands:
  build              Build libzygiskfrida.so for all Android ABIs
  download-gadget    Download Frida Gadget archives into target/frida-gadget
  package            Build out/{name}-{version}-release.zip
  flash              Package, push, and install the module with Magisk
  flash-and-reboot   Package, install, and reboot the connected device
",
        name = MODULE_NAME,
        version = module_version()
    );
}

fn package() -> Result<(), String> {
    build_rust()?;
    let gadget_dir = download_gadgets()?;

    let root = workspace_root()?;
    let out_dir = root.join("out");
    let package_dir = out_dir.join("magisk_module_release");
    if package_dir.exists() {
        fs::remove_dir_all(&package_dir)
            .map_err(|err| format!("failed to remove {}: {err}", package_dir.display()))?;
    }
    fs::create_dir_all(&package_dir)
        .map_err(|err| format!("failed to create {}: {err}", package_dir.display()))?;

    copy_template_tree(&root, &package_dir)?;
    copy_runtime_libs(&root, &package_dir)?;
    copy_gadgets(&gadget_dir, &package_dir)?;
    write_hashes(&package_dir)?;

    fs::create_dir_all(&out_dir)
        .map_err(|err| format!("failed to create {}: {err}", out_dir.display()))?;
    let zip_path = out_dir.join(format!("{}-{}-release.zip", MODULE_NAME, module_version()));
    if zip_path.exists() {
        fs::remove_file(&zip_path)
            .map_err(|err| format!("failed to remove {}: {err}", zip_path.display()))?;
    }
    zip_dir(&package_dir, &zip_path)?;
    println!("built {}", zip_path.display());
    Ok(())
}

fn build_rust() -> Result<(), String> {
    let root = workspace_root()?;
    let ndk_bin = ndk_toolchain_bin()?;

    for target in ANDROID_TARGETS {
        run_command(
            Command::new("rustup")
                .arg("target")
                .arg("add")
                .arg(target.rust_target),
            &root,
        )?;

        let linker = ndk_bin.join(format!("{}{}-clang", target.linker_prefix, TARGET_SDK));
        if !linker.exists() {
            return Err(format!("Android linker not found at {}", linker.display()));
        }
        run_command(
            Command::new("cargo")
                .arg("build")
                .arg("--package")
                .arg("zygiskfrida")
                .arg("--release")
                .arg("--target")
                .arg(target.rust_target)
                .arg("--locked")
                .env(target.linker_env, linker),
            &root,
        )?;
    }
    Ok(())
}

fn download_gadgets() -> Result<PathBuf, String> {
    let cache_dir = workspace_root()?
        .join("target")
        .join("frida-gadget")
        .join(FRIDA_VERSION);
    fs::create_dir_all(&cache_dir)
        .map_err(|err| format!("failed to create {}: {err}", cache_dir.display()))?;

    for target in ANDROID_TARGETS {
        let output = cache_dir.join(format!("libgadget-{}.so.xz", target.frida_module_arch));
        if output.exists() && output.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
            continue;
        }
        let url = format!(
            "https://github.com/frida/frida/releases/download/{version}/frida-gadget-{version}-android-{arch}.so.xz",
            version = FRIDA_VERSION,
            arch = target.frida_download_arch,
        );
        let tmp = temp_path_for(&output)?;
        println!("downloading {url}");
        run_command(
            Command::new("curl")
                .arg("--fail")
                .arg("--location")
                .arg("--retry")
                .arg("3")
                .arg("--output")
                .arg(&tmp)
                .arg(&url),
            &workspace_root()?,
        )?;
        fs::rename(&tmp, &output)
            .map_err(|err| format!("failed to install {}: {err}", output.display()))?;
    }
    Ok(cache_dir)
}

fn flash(reboot: bool) -> Result<(), String> {
    let root = workspace_root()?;
    let zip_name = format!("{}-{}-release.zip", MODULE_NAME, module_version());
    let zip_path = root.join("out").join(&zip_name);
    let adb = adb_executable()?;
    run_command(
        Command::new(&adb)
            .arg("push")
            .arg(&zip_path)
            .arg("/data/local/tmp/"),
        &root,
    )?;
    run_command(
        Command::new(&adb)
            .arg("shell")
            .arg("su")
            .arg("-c")
            .arg(format!(
                "magisk --install-module /data/local/tmp/{zip_name}"
            )),
        &root,
    )?;
    if reboot {
        run_command(Command::new(&adb).arg("shell").arg("reboot"), &root)?;
    }
    Ok(())
}

fn copy_template_tree(root: &Path, package_dir: &Path) -> Result<(), String> {
    let template_dir = root.join("template/magisk_module");
    copy_dir_recursive(
        &template_dir.join("META-INF"),
        &package_dir.join("META-INF"),
        &|content, _path| content,
    )?;
    render_template(
        &template_dir.join("module.prop"),
        &package_dir.join("module.prop"),
        &[
            ("${id}", MODULE_ID.to_string()),
            ("${name}", MODULE_NAME.to_string()),
            ("${version}", module_version()),
            ("${versionCode}", MODULE_VERSION_CODE.to_string()),
            ("${author}", MODULE_AUTHOR.to_string()),
            ("${description}", MODULE_DESCRIPTION.to_string()),
            ("${updateJson}", MODULE_UPDATE_JSON.to_string()),
        ],
    )?;
    render_template(
        &template_dir.join("customize.sh"),
        &package_dir.join("customize.sh"),
        &[("@MODULE_ID@", MODULE_ID.to_string())],
    )?;
    copy_file(
        &template_dir.join("verify.sh"),
        &package_dir.join("verify.sh"),
    )?;
    copy_file(
        &root.join("config.json.example"),
        &package_dir.join("config.json.example"),
    )?;
    Ok(())
}

fn temp_path_for(path: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("path has no file name: {}", path.display()))?;
    Ok(path.with_file_name(format!(".{file_name}.tmp")))
}

fn copy_runtime_libs(root: &Path, package_dir: &Path) -> Result<(), String> {
    let lib_dir = package_dir.join("lib");
    fs::create_dir_all(&lib_dir)
        .map_err(|err| format!("failed to create {}: {err}", lib_dir.display()))?;
    for target in ANDROID_TARGETS {
        let src = root
            .join("target")
            .join(target.rust_target)
            .join("release")
            .join(format!("lib{}.so", MODULE_ID));
        let dst = lib_dir.join(target.module_lib_name);
        copy_file(&src, &dst)?;
    }
    Ok(())
}

fn copy_gadgets(gadget_dir: &Path, package_dir: &Path) -> Result<(), String> {
    let dst_dir = package_dir.join("gadget");
    fs::create_dir_all(&dst_dir)
        .map_err(|err| format!("failed to create {}: {err}", dst_dir.display()))?;
    for target in ANDROID_TARGETS {
        let name = format!("libgadget-{}.so.xz", target.frida_module_arch);
        copy_file(&gadget_dir.join(&name), &dst_dir.join(name))?;
    }
    Ok(())
}

fn write_hashes(package_dir: &Path) -> Result<(), String> {
    let files = collect_files(package_dir)?
        .into_iter()
        .filter(|path| path.extension() != Some(OsStr::new("sha256sum")))
        .collect::<Vec<_>>();
    for file in files {
        let digest = sha256_file(&file)?;
        fs::write(
            file.with_extension(format!(
                "{}sha256sum",
                file.extension()
                    .and_then(OsStr::to_str)
                    .map(|ext| format!("{ext}."))
                    .unwrap_or_default()
            )),
            digest,
        )
        .map_err(|err| format!("failed to write hash for {}: {err}", file.display()))?;
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|err| format!("failed to open {}: {err}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format_hex(&hasher.finalize()))
}

fn format_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn zip_dir(src_dir: &Path, zip_path: &Path) -> Result<(), String> {
    let file = File::create(zip_path)
        .map_err(|err| format!("failed to create {}: {err}", zip_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut files = collect_files(src_dir)?;
    files.sort();
    for file in files {
        let name = file
            .strip_prefix(src_dir)
            .map_err(|err| format!("failed to relativize {}: {err}", file.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        zip.start_file(name, options)
            .map_err(|err| format!("failed to add {}: {err}", file.display()))?;
        let mut src =
            File::open(&file).map_err(|err| format!("failed to open {}: {err}", file.display()))?;
        io::copy(&mut src, &mut zip)
            .map_err(|err| format!("failed to zip {}: {err}", file.display()))?;
    }
    zip.finish()
        .map_err(|err| format!("failed to finish {}: {err}", zip_path.display()))?;
    Ok(())
}

fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    transform: &dyn Fn(String, &Path) -> String,
) -> Result<(), String> {
    for file in collect_files(src)? {
        let relative = file
            .strip_prefix(src)
            .map_err(|err| format!("failed to relativize {}: {err}", file.display()))?;
        let target = dst.join(relative);
        let content = fs::read_to_string(&file)
            .map_err(|err| format!("failed to read {}: {err}", file.display()))?;
        write_text(&target, &transform(content, &file))?;
    }
    Ok(())
}

fn render_template(src: &Path, dst: &Path, replacements: &[(&str, String)]) -> Result<(), String> {
    let mut content = fs::read_to_string(src)
        .map_err(|err| format!("failed to read {}: {err}", src.display()))?;
    for (from, to) in replacements {
        content = content.replace(from, to);
    }
    write_text(dst, &content)
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    let mut file =
        File::create(path).map_err(|err| format!("failed to create {}: {err}", path.display()))?;
    file.write_all(content.replace("\r\n", "\n").as_bytes())
        .map_err(|err| format!("failed to write {}: {err}", path.display()))
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    fs::copy(src, dst).map_err(|err| {
        format!(
            "failed to copy {} to {}: {err}",
            src.display(),
            dst.display()
        )
    })?;
    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_files_inner(root, &mut files)?;
    Ok(files)
}

fn collect_files_inner(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        fs::read_dir(path).map_err(|err| format!("failed to list {}: {err}", path.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|err| format!("failed to read entry in {}: {err}", path.display()))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|err| format!("failed to stat {}: {err}", path.display()))?;
        if metadata.is_dir() {
            collect_files_inner(&path, files)?;
        } else if metadata.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn ndk_toolchain_bin() -> Result<PathBuf, String> {
    let sdk = android_sdk_dir()?;
    let host = host_tag()?;
    let ndk_bin = sdk
        .join("ndk")
        .join(NDK_VERSION)
        .join("toolchains/llvm/prebuilt")
        .join(host)
        .join("bin");
    if ndk_bin.exists() {
        Ok(ndk_bin)
    } else {
        Err(format!(
            "Android NDK LLVM toolchain not found at {}",
            ndk_bin.display()
        ))
    }
}

fn adb_executable() -> Result<PathBuf, String> {
    let executable = if cfg!(windows) { "adb.exe" } else { "adb" };
    let adb = android_sdk_dir()?.join("platform-tools").join(executable);
    if adb.exists() {
        Ok(adb)
    } else {
        Err(format!("adb not found at {}", adb.display()))
    }
}

fn android_sdk_dir() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("ANDROID_HOME").or_else(|| env::var_os("ANDROID_SDK_ROOT")) {
        return Ok(PathBuf::from(path));
    }
    let local_properties = workspace_root()?.join("local.properties");
    if let Ok(contents) = fs::read_to_string(&local_properties) {
        for line in contents.lines() {
            if let Some(path) = line.strip_prefix("sdk.dir=") {
                return Ok(PathBuf::from(path.trim()));
            }
        }
    }
    Err("Android SDK path is required; set ANDROID_HOME, ANDROID_SDK_ROOT, or sdk.dir".to_string())
}

fn host_tag() -> Result<&'static str, String> {
    match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("macos", _) => Ok("darwin-x86_64"),
        ("windows", "x86_64") => Ok("windows-x86_64"),
        (os, arch) => Err(format!("unsupported build host {os}-{arch}")),
    }
}

fn workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask manifest has no workspace parent".to_string())
}

fn run_command(command: &mut Command, cwd: &Path) -> Result<(), String> {
    println!("running: {:?}", command);
    let status = command
        .current_dir(cwd)
        .status()
        .map_err(|err| format!("failed to run {:?}: {err}", command))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} exited with {status}", command))
    }
}

fn module_version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}
