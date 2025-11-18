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
wvdsh build push
```

### Dev Sandbox Server

```bash
wvdsh dev serve [--config ./wavedash.toml]
```

What it does:
- Serves your `upload_dir` over HTTPS on a random localhost port with permissive CORS for wavedash.gg.
- Generates (and reuses) self-signed certs stored under your OS config dir (e.g. `~/.config/wvdsh/dev-server/`). The CLI will prompt to auto-trust the cert (macOS via `sudo security add-trusted-cert`, Windows via `certutil`, Linux via `sudo cp ... && update-ca-certificates` when available).
- Reads `wavedash.toml` to determine engine, version, entrypoint, and discovery of HTML exports (for Godot/Unity it scrapes the HTML to compute `entrypointParams`).
- Prints a ready-to-click sandbox link that looks like `https://wavedash.gg/play/{game_slug}?branch_slug=...&localOrigin=https://localhost:{port}&sandbox=true&engine=...`.

Tip: run the command from the repo root so the default `./wavedash.toml` is picked up automatically. Use `--config /path/to/wavedash.toml` only when needed.

## Wavedash Toml

All configs require these fields:
- `org_slug`
- `game_slug`
- `branch_slug`
- `upload_dir`

Then include exactly one engine section: `[godot]`, `[unity]`, or `[custom]`.

### Godot `wavedash.toml`

```
org_slug = "franz-labs-inc"
game_slug = "pgrc"
branch_slug = "internal-1"
upload_dir = "./builds/webgl"

[godot]
version = "4.5-stable"
```

### Unity `wavedash.toml`

```
org_slug = "franz-labs-inc"
game_slug = "pgrc"
branch_slug = "internal-1"
upload_dir = "./builds/webgl"

[unity]
version = "6000.0.2f1"
```

### Custom Engine `wavedash.toml`

```
org_slug = "franz-labs-inc"
game_slug = "pgrc"
branch_slug = "internal-1"
upload_dir = "./builds/webgl"

[custom]
version = "0.0.1"
entrypoint = "x"
```



