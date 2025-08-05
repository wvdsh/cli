# Wavedash CLI

Cross-platform CLI tool for uploading game projects to wavedash.gg.

## Development

**Local development (default):**
```bash
cargo run          # Uses config/dev.toml (localhost:5173)
```

**Staging:**
```bash
ENV=staging cargo run    # Uses config/staging.toml
```

**Production:**
```bash
ENV=prod cargo build --release    # Uses config/prod.toml
```

## Commands

```bash
wvdsh auth login          # Browser-based login
wvdsh auth login --token <key>  # Manual token
wvdsh auth logout         # Clear credentials  
wvdsh auth status         # Check auth status
```
