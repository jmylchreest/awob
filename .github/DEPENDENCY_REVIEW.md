# Dependency integration notes

The September 2026 integration combines PRs #22–29. The next release must
also account for features already merged after v0.1.7. No version or tag is
changed by the integration.

## Security follow-ups

- `RUSTSEC-2026-0192` is an informational unmaintained notice for `ttf-parser`.
  It remains transitive through `cosmic-text 0.19` → `fontdb 0.23`; there is no
  patched version. `deny.toml` makes a specific exception for this notice,
  while continuing to block other advisories. Revisit when either upstream
  releases a replacement. See <https://rustsec.org/advisories/RUSTSEC-2026-0192>.
- The locked Docs dependency graph still reports npm advisories after
  compatible `npm audit fix`: image-size (GHSA-w3rx-r6r6-pgpr and
  GHSA-5p2g-fcmc-qvqq), serialize-javascript (GHSA-5c6j-r48x-rmvq and
  GHSA-qj8w-gfj5-8c6v), and uuid (GHSA-w5hq-g745-h8pq). npm reports no fix
  through the current Docusaurus dependency ranges. These dependencies
  belong to the documentation build/development tooling, not the Rust
  binaries. Do not expose the development server to untrusted clients.
  Revisit with a Docusaurus update; do not force incompatible transitive
  major versions just to suppress the audit. PR builds have read-only
  repository permissions and cannot deploy Pages.

## Before release

- Require passing CI, Docs build and Release artifact verification on the
  integration commit. Release PR runs build and validate artifacts but skip
  both publishers; manual dispatch still publishes a snapshot.
- Smoke-test speaker/microphone volume and mute, brightness listeners,
  theme reload (including atomic editor saves), icon themes and rendering
  on a Wayland session with the relevant hardware.
- Configure main branch protection separately so required checks are
  enforced. Repository settings are not changed by this PR.
- Close superseded PRs #22–29 after the integration PR merges.
