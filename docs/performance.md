# Performance checks

Run the renderer benchmarks with `cargo bench -p awob-core --bench render`.
Divan reports time and allocation traffic for warmed renderers using four bundled
themes. Font selection still depends on installed fonts; use the same machine and
font set for comparisons. The inline icon removes icon-theme lookup variability.
Allocation profiling adds overhead, so compare like-for-like runs.

To compare the size-oriented renderer settings without editing Cargo.toml:

```sh
cargo bench -p awob-core --bench render \
  --config 'profile.release.package.awob-core.opt-level="z"' \
  --config 'profile.release.package.tiny-skia.opt-level="z"' \
  --config 'profile.release.package.tiny-skia-path.opt-level="z"' \
  --config 'profile.release.package.resvg.opt-level="z"'
```

Release builds optimise awob-core, tiny-skia, tiny-skia-path and resvg for speed.
The workspace default remains size optimisation for clients and listeners. This
increases the daemon binary size; measure the stripped daemon alongside rendering
cost when changing these overrides. Benchmark results describe CPU rasterisation,
not compositor frame pacing or IPC latency.

The Renderer benchmarks workflow saves timing and allocation reports for relevant
PRs and main-branch changes. Timing is observational rather than a pass/fail
threshold because shared CI machines vary; pixel and allocation regression tests
provide deterministic correctness checks.

On a Ryzen 7 9800X3D with Rust 1.98.1, this benchmark measured default frames at
618 microseconds with all four overrides set to `z`, versus 146 microseconds
with the selective speed settings. The stripped daemon grew from 5,711,344 to
5,845,984 bytes (2.4%). These measurements precede the separate render-storage
reuse change and are not a promise of the same speedup on every machine.

The suite includes `warm_frame` for the owned-pixmap API and `warm_cached_frame` for the borrowed frame used by the daemon. Both reuse one warmed renderer and a fixed inline icon across four shipped themes. The earlier profile comparison above predates the renderer caches.
