## Project Purpose
- The goal of this repository is to create the wavedash cli, which helps game developers upload their assets to the site (similar to steams "steampipe" cli).

## Development
- Environment variables are managed by Doppler. Always use `doppler run --` as a prefix when running cargo commands (build, check, clippy, run, test, etc.). For example: `doppler run -- cargo check`, `doppler run -- cargo clippy`.
- To run the CLI locally, use `doppler run -- cargo run <command>`. For example: `doppler run -- cargo run build push`.