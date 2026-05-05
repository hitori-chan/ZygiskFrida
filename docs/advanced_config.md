# Advanced Config

For the previous configuration method with various files, see [simple config](simple_config.md).
It remains a valid method of configuration but the structured configuration method specified here is the preferred
method in the future and also supports more features.

Both configuration are supported with the advanced config taking precedence in case an app appears in both.

## Config File

This module is configured via a json config located at `/data/adb/modules/zygiskfrida/config.json`.
To start off, you can copy the example config
```shell
adb shell 'su -c cp /data/adb/modules/zygiskfrida/config.json.example /data/adb/modules/zygiskfrida/config.json'
```

Example config
```json
{
    "targets": [
        {
            "app_name" : "com.example.package",
            "enabled": true,
            "start_up_delay_ms": 0,
            "stage_libraries_in_app_data": true,
            "injected_libraries": [
                {
                    "path": "libgadget.so"
                }
            ],
            "child_gating": {
                "enabled": false,
                "mode": "freeze",
                "injected_libraries" : [
                    {
                        "path": "libgadget-child.so"
                    }
                ]
            }
        }
    ]
}
```

The config contains an array of targets. A target contains the configuration for one application
you want to inject with frida.

In case things are not working as expected, check `adb logcat -s ZygiskFrida` to see if an error is logged.

## Target configuration.

### app_name
The bundle id of the application you want to inject frida into.

### enabled
If set to false, then this module will ignore this configuration.
This is useful if you want to temporarily disable a target while maintaining the config.


### start_up_delay_ms
Injection of libraries is delayed by this amount in milliseconds.

There are times that you might want to delay the injection of the gadget. Some applications
might run checks at start up and delaying the injection can help avoid these.

### stage_libraries_in_app_data
When enabled, ZygiskFrida copies each configured library into
`<app_data_dir>/files/zygiskfrida/` before app specialization and injects the
staged copy after specialization. The staged files are owned by the target app UID.

This is recommended for Frida Gadget on modern Android because the gadget opens
its sidecar config file after the process is already running under the app
sandbox. A gadget loaded from `/data/adb/modules/zygiskfrida/libgadget.so` may be
able to execute while still failing to read
`/data/adb/modules/zygiskfrida/libgadget.config.so`.

If a matching sidecar config exists next to the configured library, for example
`libgadget.config.so` next to `libgadget.so`, it is staged with the library. If
no sidecar exists for a `libgadget*` library, ZygiskFrida writes a default
listen-mode config with `on_load` set to `resume`.

### injected_libraries
These are the libraries that will be injected into the process. The libraries
specified here will be loaded in the order of the array.

The module includes a bundled frida gadget as `libgadget.so` in the Magisk module directory.\
Relative library paths are resolved against `/data/adb/modules/zygiskfrida` before app specialization.

For convenience this module also installs a gadget at `libgadget32.so` for injection into application
with 32-bit only support on 64-bit devices.

You can adjust the gadget config according to the official [Gadget Doc](https://frida.re/docs/gadget/)

If you want to use a different frida version or an alternative version you can replace this
with the path to your own gadget.

Using this you can also inject arbitrary libraries alongside the gadget or without the gadget if
you remove it.
Absolute paths are still accepted, but relative paths are preferred because the module opens them through
Zygisk's module directory fd before app sandboxing is applied.

The module will setup file permissions in the module directory on install. If you suspect a file permission
issue, place your libraries within `/data/adb/modules/zygiskfrida` and reinstall the module.


## Child gating configuration (experimental)
This is an experimental feature and has a lot of caveats! Please read carefully.

This module is able to intercept fork/vfork within the process to instrument child processes.
An application might fork a child process to run checks from there that you can't intercept
without child gating.

By enabling this feature by setting `enabled` to true, you can configure how to deal
with these child processes.

There are currently 3 modes in how child gating operates. You can determine by
setting the mode to either `freeze`, `kill` or `inject`.

Using any of the child gating mode can cause issues properly shutting down the application even with a force close.
This can cause issues restarting the app. Manually killing the app can resolve this.
```
adb shell 'su -c kill -9 $(pidof com.example.package)'
```

### freeze
The child process will not return from the fork. This means that no code will
run within the child process but the process itself stays alive.

### kill
The child process will be killed as soon as it is forked. No code will
run within the child process.

### inject
This mode will inject the `injected_libraries` into the child process similiar to the target configuration.
After injection the child process will resume its normal code flow. You may fail to connect to the gadget
interactively if the child is only doing a quick check and exits.

Please be aware as the child is forked, it already contains all libraries loaded that the parent processs had.
But as only a single thread returns from the fork the loaded frida gadget thread is not present in the child process.

Reloading the same bundled gadget will fail to start. For this to work you have to load a copy of the gadget.
You can't load the same file into the process again, a symbolic link won't work either it must be a copy.
F.e.

```shell
adb shell 'su -c cp /data/adb/modules/zygiskfrida/libgadget.so /data/adb/modules/zygiskfrida/libgadget-child.so'
```

The default configuration of a gadget will fail to start due to port conflict with the gadget in the parent process.
So for the child process you would have to configure the gadget to use a different port.

Create a gadget configuration like this at `/data/adb/modules/zygiskfrida/libgadget-child.config.so`.
See [Gadget Doc](https://frida.re/docs/gadget/) for reference.
```
{
  "interaction": {
    "type": "listen",
    "address": "127.0.0.1",
    "port": 27043,
    "on_port_conflict": "pick-next",
    "on_load": "wait"
  }
}
```

Please take note of the `on_port_conflict: pick-next` which is important in case the parent process forks
multiple children.

As this is a non-default port gadget you can take a look at `adb logcat -s Frida` to see which ports the
child gadget started on.

Then you can connect it for example via
```shell
adb forward tcp:27043 tcp:27043
frida -H 127.0.0.1:27043 -n Gadget
```
