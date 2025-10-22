# Wavedash CLI

Cross-platform CLI tool for uploading game projects to wavedash.gg.

## Installation

```bash
cargo install wavedash
```

## Development

Copy `config.toml.example` to `config.toml` and customize for local development:

```bash
cp config.toml.example config.toml
# Edit config.toml to point to your local/staging environment
cargo run
```

The CLI defaults to production config when `config.toml` doesn't exist (for published releases).

### Building

```bash
cargo build --release
```

## Commands

### Authentication

```bash
wvdsh auth login          # Browser-based login
wvdsh auth login --token <key>  # Manual token
wvdsh auth logout         # Clear credentials  
wvdsh auth status         # Check auth status
```

### Build Management

```bash
wvdsh build push <org_slug>/<game_slug>:<branch> -e <engine> -v <version> [source_dir]

# Example:
wvdsh build push myorg/mygame:main -e godot -v 4.3 ./build
```


