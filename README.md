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

### Unity `wavedash.toml`

```
game_slug = "ski-trooper"
branch_slug = "production"
upload_dir = "./builds/webgpu"

[engine]
type = "unity"
version = "6000.0.2f1"
```

### Custom Engine `wavedash.toml`

```
game_slug = "ski-trooper"
branch_slug = "production"
upload_dir = "./builds/webgpu"

# wavedash.toml for aground
[engine]
type = "custom"
version = "0.0.1"
entrypoint = "web-entrypoint.js"
```

### Godot `wavedash.toml`

```
game_slug = "ski-trooper"
branch_slug = "production"
upload_dir = "./builds/webgpu"

[engine]
type = "godot"
version = "4.5-stable"
```



