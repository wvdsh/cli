# Wavedash CLI

Upload and manage games on [Wavedash](https://wavedash.com).

## Install

macOS / Linux:

```sh
curl -fsSL https://wavedash.com/cli/install.sh | sh
```

Windows (PowerShell):

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass; irm https://wavedash.com/cli/install.ps1 | iex
```

Homebrew:

```sh
brew install wvdsh/tap/wavedash
```

Binaries are also on [Releases](https://github.com/wvdsh/cli/releases).

## Usage

```sh
wavedash auth login
wavedash init
wavedash build push
```

All commands: [docs.wavedash.com/cli](https://docs.wavedash.com/cli)

## Build

`cargo build --release`, with `SITE_HOST`, `CONVEX_HTTP_URL` and `PLAYSITE_HOST` set.
