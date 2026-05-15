# Wavedash Dev

Electron host that powers `wavedash dev`. **Not a standalone app** — the CLI
downloads a platform-specific zip from the GitHub Release per CLI version
and execs the binary directly.

## Layout

- `src/main.ts` — process entry. Reads JSON config from the temp file at
  `--config-path=<path>` (CLI also passes `--user-data-dir` and
  `--parent-pid`), starts the local HTTPS server, points Chromium at it
  via `--host-rules`, and navigates to the playtest URL.
- `src/server.ts` — local HTTPS server. Handles requests for any
  `{gcid}-{userhash}.<localHostSuffix>` host: synthesizes the
  `/dev-app-embed` shell, serves files from the upload dir with
  COEP/COOP/CORP, and reverse-proxies a fixed set of paths (`/embed.js`,
  `/sw-bootstrap`, `/auth/refresh`, …) to the real network.
- `src/cert.ts` — generates a self-signed cert at startup with a wildcard
  SAN `*.<localHostSuffix>`. The cert is whitelisted via
  `session.setCertificateVerifyProc` for any host under that suffix only,
  so DevTools network is unaffected.
- `electron-builder.json5` — produces `<productName>-<version>-<os>-<arch>.zip`
  per platform. The release workflow renames each to `<platform>.zip`.

## Why a local server, not CDP `Fetch.enable`

Electron's `webContents.debugger` and bundled DevTools share a single CDP
slot. While the debugger is attached with `Fetch.enable`, the bundled
DevTools' Network tab is broken (and opening DevTools detaches our
debugger). Routing the wildcard `*.<localHostSuffix>` through a real local
HTTPS server keeps Chromium's network stack untouched, so right-click →
Inspect Element opens DevTools with full Network/Performance panels.

## No code signing

Builds are unsigned on every platform. The CLI downloads via `reqwest`, which
doesn't set `com.apple.quarantine` (macOS) or Mark-of-the-Web (Windows), and
launches the binary as a child process — so neither Gatekeeper nor SmartScreen
ever runs against it. Linux has no signing concept.

Tradeoffs to know:

- A dev who manually opens `Wavedash Dev.app` from Finder will get a
  Gatekeeper warning. The dev-app isn't meant to be run standalone, so this
  is fine.
- If Apple ever extends quarantine to programmatic downloads, unsigned breaks
  overnight. Notarization is the only forward-compatible fix; reintroduce by
  adding `APPLE_ID` / `APPLE_APP_SPECIFIC_PASSWORD` / `APPLE_TEAM_ID` /
  `CSC_LINK` / `CSC_KEY_PASSWORD` secrets, removing `identity: null` from
  `electron-builder.json5`, and re-adding `notarize: true`.

## Version sync invariant

The dev-app version is **identical** to the CLI version. See
`cli/src/dev/dev_app.rs::DEV_APP_VERSION` — it pulls from `CARGO_PKG_VERSION`,
so every CLI tag publishes a matching dev-app build under the same GitHub
Release. Bumping the CLI without also publishing the matching dev-app zip
breaks `wavedash dev` for everyone on the new CLI.

`dev-app-release.yml` patches `package.json`'s `version` field to the tag at
build time, so the version in this repo is just a default — the real version
is whatever tag the workflow runs against.

## Iterating locally

The CLI checks the `DEV_APP_DEV_PATH` env var first; if set, it skips the
download and launches via `npx electron .` against that path. To iterate on
the dev-app source:

```bash
cd cli/dev-app
bun install
bun run build           # bundles src/main.ts → dist/main.js (~12ms)

# In another shell:
DEV_APP_DEV_PATH=/abs/path/to/cli/dev-app doppler run -- cargo run -- dev
```

Re-run `bun run build` whenever you edit `src/`. The CLI re-launches
electron each `wavedash dev` run, so the rebuilt code picks up automatically.

`bun run typecheck` runs `tsc --noEmit` for IDE-style type checking — Bun's
bundler doesn't typecheck, it just transpiles.

## IPC contract

CLI → dev-app via the temp file at `--config-path=<path>` — one JSON
object. Stdin is intentionally unused (Windows GUI-subsystem .exes detach
from inherited stdin pipes):

```json
{
  "uploadDir": "/abs/path",
  "localHostSuffix": "local.wavedashcdn.com",
  "playtestUrl": "https://wavedash.com/playtest/<slug>/<uuid>",
  "verbose": false
}
```

The dev-app reads it synchronously at startup and unlinks the file.
`localHostSuffix` is the **shared suffix** for every iframe origin the
mainsite may navigate to (`{gcid}-{userhash}.<localHostSuffix>`); the
dev-app uses a wildcard for cert SAN, `--host-rules` MAP, and the
cert-verify check.

Dev-app → CLI over **stdout** — one JSON object per line:

- `{"type":"ready"}` after first `did-finish-load`.
- `{"type":"closed"}` immediately before `app.quit()`.

stderr: server access log (one line per intercepted request) plus errors.
Always forwarded by the CLI; `--verbose` only adds CLI-side noise.

Shutdown: on Ctrl+C the CLI kills the child directly. The dev-app also
polls `--parent-pid=<pid>` every second and quits if the CLI process is
gone, so a CLI crash doesn't leave an orphan.
