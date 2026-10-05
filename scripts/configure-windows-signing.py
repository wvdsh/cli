"""Configure cargo-dist from the GitHub release environment; never store credentials."""

import json
import os
from pathlib import Path
import re
import tomllib
from urllib.parse import urlparse


def configure(path: Path) -> None:
    fields = {
        "endpoint": "WINDOWS_SIGNING_ENDPOINT",
        "account-name": "WINDOWS_SIGNING_ACCOUNT",
        "certificate-profile-name": "WINDOWS_SIGNING_PROFILE",
    }
    values = {key: os.environ.get(env, "").strip() for key, env in fields.items()}
    missing = [fields[key] for key, value in values.items() if not value]
    if missing:
        raise ValueError("Missing GitHub release environment variables: " + ", ".join(missing))
    endpoint = urlparse(values["endpoint"])
    if (
        endpoint.scheme != "https"
        or not (endpoint.hostname or "").endswith(".codesigning.azure.net")
        or endpoint.username
        or endpoint.password
        or endpoint.query
        or endpoint.fragment
        or endpoint.path not in ("", "/")
        or endpoint.port not in (None, 443)
    ):
        raise ValueError("WINDOWS_SIGNING_ENDPOINT must be an Azure HTTPS signing endpoint")
    original = path.read_text(encoding="utf-8")
    section = "[dist.azure-windows-sign]"
    before, separator, after = original.partition(section)
    if not separator:
        raise ValueError("Missing cargo-dist Azure signing configuration")
    for key, value in values.items():
        after, count = re.subn(
            rf'(?m)^{re.escape(key)} = "CONFIGURED_IN_CI"$',
            lambda _: f"{key} = {json.dumps(value, ensure_ascii=False)}",
            after,
        )
        if count != 1:
            raise ValueError(f"Expected exactly one unconfigured {key} marker")
    result = before + separator + after
    if tomllib.loads(result)["dist"]["azure-windows-sign"] != values:
        raise ValueError("Generated signing configuration did not match environment")
    path.write_text(result, encoding="utf-8")


if __name__ == "__main__":
    configure(Path(__file__).resolve().parent.parent / "dist-workspace.toml")
