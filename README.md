# ZygiskFrida

> [Frida](https://frida.re) is a dynamic instrumentation toolkit for developers, reverse-engineers, and security researchers

> [Zygisk](https://github.com/topjohnwu/Magisk) is part of Magisk and allows you to run code in every Android application's process.


## Introduction

[ZygiskFrida](README.md) is a Zygisk module allowing you to inject Frida Gadget in Android applications in a
more controlled way.

- The gadget is not embedded into the APK itself. So APK Integrity/Signature checks will still pass.
- The process is not being ptraced like it is with frida-server. Avoiding ptrace based detection.
- Control over the injection time of the gadget.
- Allows you to load multiple arbitrary libraries into the process.

## How it works

The module zip contains two separate native components:

- `libzygiskfrida.so` is built directly from the Rust crate in this codebase and loaded by Zygisk.
- `libgadget.so` is the bundled Frida Gadget loaded into configured target apps.

During app specialization, ZygiskFrida reads its config from the Magisk module directory
(`/data/adb/modules/zygiskfrida`) using Zygisk's module directory file descriptor. Relative
library paths such as `libgadget.so` are resolved against that directory before the app sandbox is
applied, then injected later through file descriptors. Runtime assets are not installed into public
temporary storage.

## How to use the module

### Prerequisites
- Rooted device/emulator
- Zygisk available and enabled

### Quick start
- Download the latest release from the [Release Page](https://github.com/hitori-chan/ZygiskFrida/releases).
- Transfer the ZygiskFrida zip file to your device and install it via Magisk.
- Reboot after install
- Create the config file and adjust the package name to your target app (replace `your.target.application` in the commands)
```shell
adb shell 'su -c cp /data/adb/modules/zygiskfrida/config.json.example /data/adb/modules/zygiskfrida/config.json'
adb shell 'su -c sed -i s/com.example.package/your.target.application/ /data/adb/modules/zygiskfrida/config.json'
```
- The default config stages the gadget under `<app_data_dir>/files/.zygiskfrida`
  using generated file names so Frida can read its sidecar config after app
  sandboxing without exposing the source library basename in staged files.
- Launch your app and attach to the gadget
  e.g. `frida -U -N your.target.application` or `frida -U -n Gadget`

This assumes that you don't have any other frida server running, e.g. by using MagiskFrida.
You can still run it together with frida-server but you would have to configure the gadget
to use a different port.

### Configuration

Configuration uses the v2 JSON schema in `config.json`. It supports startup
delay, arbitrary library load order, app-data staging, and child gating. Use
relative library paths when the libraries are stored in the module directory.
This keeps the original module direction of delayed gadget injection, loading
arbitrary libraries, and child gating while using the current structured config.

Please take a look at the [configuration guide](docs/advanced_config.md) for this.

## Surface cleanup scope

The v2 runtime defaults to app-data staging with generated staged names under
`<app_data_dir>/files/.zygiskfrida`. This reduces artifacts created by
ZygiskFrida itself, such as basename-bearing staged files and path-heavy release
diagnostics.

This cleanup is intentionally narrow. It does not hide Frida's protocol, ports,
Gadget behavior, Magisk module identity, package layout, syscalls, `/proc`
visibility, or app/security checks.

## How to build

- Checkout the project
- Install Rust, `curl`, and Android SDK/NDK `25.2.9519653`.
- Set `ANDROID_HOME`, `ANDROID_SDK_ROOT`, or `sdk.dir` in `local.properties`.
- Run `cargo xtask package`
- The built Magisk module should then be in the `out` directory.

You can also build and install the module to your device directly with
`cargo xtask flash-and-reboot`.

## Caveats

- For emulators this will start the gadget in native realm. This means that you will be able to hook Java but not native functions.
- This is not yet tested very well on different devices. In case this is not working, reports with logs from `adb logcat -s ZygiskFrida` are welcome.

## Credits

- Inspired by [Zygisk-Il2CppDumper](https://github.com/Perfare/Zygisk-Il2CppDumper)
- Historical versions used [xDL](https://github.com/hexhacking/xDL)
