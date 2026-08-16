# Bindings

Per the spec, the C ABI (`slimdb_*`) is the fundamental contract; all bindings
sit on it, never on the core's internal structures.

| Binding | Status | Location |
|---|---|---|
| C | ✅ v1 | `bindings/c` (header, staticlib/cdylib, example, tests) |
| Python | ✅ v1 (PyO3 on the C ABI) | `bindings/python` (`pip install .`) |
| Node | planned | N-API on the C ABI |
| Godot | planned | GDExtension; demo must survive `kill -9` mid-write |

The engine passed the crash-test loop (500+ SIGKILL iterations, 0 divergence)
before any binding work started, per spec §10/§13.

## Conventions (v1)

- Handles are opaque; the caller owns their lifetime.
- Values cross every boundary as JSON strings — the API model, never the
  storage format.
- Strings returned by the C ABI are freed with `slimdb_string_free` /
  `slimdb_string_list_free`.
- No panic ever crosses the ABI: every export reports `SLIMDB_ERR_PANIC`
  instead.
- The C ABI version (`slimdb_abi_version()`) is bumped only on breaking
  changes; bindings must check it at load time.
