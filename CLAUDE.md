## Project Purpose
- The goal of this repository is to create the wavedash cli, which helps game developers upload their assets to the site (similar to steams "steampipe" cli).

## Development
- Environment variables are managed by Doppler. Always use `doppler run --` as a prefix when running cargo commands (build, check, clippy, run, test, etc.). For example: `doppler run -- cargo check`, `doppler run -- cargo clippy`.

## JS SDK
- `wavedash dev` injects `@wvdsh/sdk-js` from jsdelivr into the boot shell. The version is pinned in `src/dev/sdk-js-version` (one line, `include_str!`'d by `src/dev/server.rs`) — never `@latest`, so an SDK release can't change dev behaviour underfoot. Bump it with `./scripts/bump-sdk-js.sh` (no args = current npm `latest`), and keep it in step with the version play bundles in prod (`play/package.json`).
