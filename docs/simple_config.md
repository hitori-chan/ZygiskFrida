# Simple Config

The previous file-based configuration using `target_packages` and
`injected_libraries` is no longer loaded by ZygiskFrida.

Use the v2 JSON config at `/data/adb/modules/zygiskfrida/config.json` instead:

```shell
adb shell 'su -c cp /data/adb/modules/zygiskfrida/config.json.example /data/adb/modules/zygiskfrida/config.json'
```

Equivalent v2 config for a legacy `target_packages` entry of
`com.example.package,20000` with the default Gadget:

```json
{
  "config_version": 2,
  "targets": [
    {
      "app_name": "com.example.package",
      "enabled": true,
      "start_up_delay_ms": 20000,
      "staging": "app_data",
      "injected_libraries": [
        {
          "path": "libgadget.so"
        }
      ]
    }
  ]
}
```

For arbitrary libraries, add them to `injected_libraries` in load order. Relative
paths are resolved against the module directory before app sandboxing is applied.
With the default `app_data` staging mode, ZygiskFrida copies those libraries into
`<app_data_dir>/files/.zygiskfrida` using generated names like
`0000-<hash>.so` and `0000-<hash>.config.so`; the staged names do not include
the source library basename.

This only reduces ZygiskFrida-created staging artifacts. It does not hide Frida
protocol traffic, ports, Gadget behavior, module identity, syscalls, `/proc`, or
app/security checks.
