## Project Purpose
- The goal of this repository is to create the wavedash cli, which helps game developers upload their assets to the site (similar to steams "steampipe" cli).

## Development
- Environment variables are managed by Doppler. Always use `doppler run --` as a prefix when running cargo commands (build, check, clippy, run, test, etc.). For example: `doppler run -- cargo check`, `doppler run -- cargo clippy`.

## JS SDK
- `wavedash dev` injects `@wvdsh/sdk-js` from jsdelivr into the boot shell. The version is pinned in `src/dev/sdk-js-version` (one line, `include_str!`'d by `src/dev/server.rs`) — never `@latest`, so an SDK release can't change dev behaviour underfoot. Bump it with `./scripts/bump-sdk-js.sh` (no args = current npm `latest`), and keep it in step with the version play bundles in prod (`play/package.json`).

## Releases
- Pushing a `Cargo.toml` version bump to `main` makes `.github/workflows/auto-tag.yml` tag the commit, which is what triggers `release.yml`. Before tagging, it refuses to release when `src/dev/sdk-js-version` doesn't match the `@wvdsh/sdk-js` version play serves in production — read from `package-lock.json` at play's latest GitHub release, since play deploys prod on `release: published`. To clear a failure: `./scripts/bump-sdk-js.sh <version>`, verify a game boots with `wavedash dev`, then push the bump. The gate never bumps the pin itself, because a new SDK wants that manual pass.
- Reading a private repo from this public one goes through the **play-reader GitHub App**, not a personal token. One-time setup, if the secrets ever need reissuing:
  1. In the `wvdsh` org, create a GitHub App named `play-reader`, repository permission `Contents: Read-only`, and nothing else. Uncheck Webhook → Active.
  2. Install it on the `wvdsh` org, choosing **Only select repositories → play**.
  3. Generate a private key, then set `PLAY_READER_APP_CLIENT_ID` (the App's Client ID) and `PLAY_READER_APP_PRIVATE_KEY` (the whole `.pem`, including the BEGIN/END lines) as Actions secrets on this repo.
- `actions/create-github-app-token` then mints an installation token per run, scoped to `wvdsh/play` alone and revoked when the job ends — so nothing long-lived sits in a public repo, and the token can't reach any other private repo even if it leaked. Note the App still grants read of all of play's source to anyone who can merge to this repo's `main`; if that ever needs tightening, have play publish the shipped SDK version somewhere public (an npm `prod` dist-tag, or a version endpoint on the deployed site) and drop the credential entirely.
