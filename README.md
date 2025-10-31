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



