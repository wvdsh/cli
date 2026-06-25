Download the latest [release](https://github.com/wvdsh/cli/releases).

## Authentication

For a local desktop session, sign in with the browser flow:

```bash
wavedash auth login
```

Browser login opens a local callback server and requires a desktop browser. In
CI/CD or other headless environments, use a token instead:

```bash
export WAVEDASH_TOKEN=wd_...
wavedash auth status --json
```

`WAVEDASH_TOKEN` is the recommended authentication path for automation. It takes
precedence over stored credentials in `~/.wavedash/credentials.json`.

To store a token locally without putting it in shell history or process
arguments:

```bash
printf "%s" "$WAVEDASH_TOKEN" | wavedash auth login --token-stdin
```

## Automation

For scripts and CI, prefer machine-readable output and disable terminal-only
behavior:

```bash
wavedash --no-color --no-update-check init --team-name "My Studio" --game-title "My Game" --upload-dir dist --engine custom --force --json
```

Use `--json` for structured output. JSON commands suppress update notices by
default. Use `--no-color` or the standard `NO_COLOR` environment variable to
disable ANSI color output. Use `--no-update-check` or
`WAVEDASH_NO_UPDATE_CHECK=1` to disable background update checks.

`wavedash init` is interactive when run without flags. For scripts, pass
explicit flags instead. Non-interactive init requires `--team-id` or
`--team-name`, and `--game-id` or `--game-title`. Pass `--force` to overwrite an
existing `wavedash.toml`.

Run `wavedash <command> --help` for command-specific options.
