# Bindings

Per the spec, the C ABI (`slimdb_*`) is the fundamental contract; all bindings
sit on it, never on the core's internal structures.

Order of arrival (roadmap, Phase 3+):

| Binding | Tech | Gate |
|---|---|---|
| C | the ABI itself | `slimdb_open/close/get/set/delete/begin/commit/rollback` |
| Python | PyO3 | `pip install slimdb` without local compilation |
| Node | N-API | `npm install slimdb` works |
| Godot | GDExtension | demo survives `kill -9` mid-write |

The engine must pass the crash-test loop stably **before** any binding work
starts (spec §10, §13).
