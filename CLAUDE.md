## Project Purpose
- The goal of this repository is to create the wavedash cli, which helps game developers upload their assets to the site (similar to steams "steampipe" cli).

## Development
- Environment variables are managed by Doppler. Always use `doppler run --` as a prefix when running cargo commands (build, check, clippy, run, test, etc.). For example: `doppler run -- cargo check`, `doppler run -- cargo clippy`.

## JS SDK
- `wavedash dev` injects `@wvdsh/sdk-js` from jsdelivr into the boot shell. The version is pinned in `src/dev/sdk-js-version` (one line, `include_str!`'d by `src/dev/server.rs`) — never `@latest`, so an SDK release can't change dev behaviour underfoot. Bump it with `./scripts/bump-sdk-js.sh` (no args = current npm `latest`), and keep it in step with the version play bundles in prod (`play/package.json`).

## Releases
- Pushing a `Cargo.toml` version bump to `main` makes `.github/workflows/auto-tag.yml` tag the commit, which is what triggers `release.yml`. Before tagging, it refuses to release when `src/dev/sdk-js-version` doesn't match the `@wvdsh/sdk-js` version play serves in production — read from `package-lock.json` at play's latest GitHub release, since play deploys prod on `release: published`. To clear a failure: `./scripts/bump-sdk-js.sh <version>`, verify a game boots with `wavedash dev`, then push the bump. The gate never bumps the pin itself, because a new SDK wants that manual pass.
- Reading `wvdsh/play` (private) from this public repo uses the `PLAY_READ_PAT` Actions secret: a fine-grained personal access token scoped to only `wvdsh/play` with repository permission `Contents: Read-only`. If it ever needs reissuing: GitHub → Settings → Developer settings → Fine-grained tokens, resource owner `wvdsh`, only-select-repositories → `play`, Contents read-only, then update the secret on this repo. Fine-grained PATs expire, so the workflow will start failing when it lapses — reissue and update the secret. It's also tied to the account that created it; if that account leaves the org, reissue from another account, or switch to a per-run GitHub App installation token if this ever needs to stop depending on a person.
