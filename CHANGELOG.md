# Changelog

## v1.10.2

- Reworked runtime configuration around the v2 JSON schema.
- Staged app-data libraries under `<app_data_dir>/files/.zygiskfrida`.
- Replaced basename-bearing staged files with deterministic generated names.
- Added stale generated-file cleanup and one-time cleanup for old generated
  `files/zygiskfrida/0000-*` staging files.
- Reduced release log path detail by using stable library labels such as
  `parent[0]` and `child[0]`.
- Kept Frida Gadget filenames, module identity, attach behavior, ports, and
  protocol behavior unchanged.
