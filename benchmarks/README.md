# Benchmarks

Per the spec's guardrail: **no performance figure is published without being
measured on the real target platform** — numbers from other projects are
never copied.

Planned measurements (Phase 6):

- writes/sec per durability mode (`safe` / `balanced` / `fast`) on
  desktop Linux/macOS/Windows
- fsync cost on constrained targets (mobile exports, Switch via Godot) —
  measured per platform, documented per platform
- open/reopen latency at 1k / 10k / 100k documents
- compact time vs WAL size

No numbers are claimed in the README until these runs exist.
