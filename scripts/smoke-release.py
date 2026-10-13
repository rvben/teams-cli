"""Verify an extracted release executable with isolated, credential-free profiles."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    binary = str(Path(sys.argv[1]).resolve())
    version = sys.argv[2]
    with tempfile.TemporaryDirectory(prefix="teams-release-smoke-") as directory:
        environment = {
            key: value for key, value in os.environ.items()
            if not key.upper().startswith("TEAMS_")
        }
        environment["XDG_CONFIG_HOME"] = directory

        def run(*arguments, exit_code=0):
            result = subprocess.run(
                [binary, *arguments], env=environment, capture_output=True,
                text=True, timeout=20,
            )
            if result.returncode != exit_code:
                raise RuntimeError(f"{arguments[0]}: unexpected exit {result.returncode}")
            return result.stdout if exit_code == 0 else result.stderr

        # The short form is a stable package version; the long form also identifies
        # the build. Verify both without accepting a different package version.
        short_version = f"teams {version}"
        long_version = run("--version").strip()
        if (run("-V").strip() != short_version or
                (long_version != short_version and not (
                    long_version.startswith(short_version + " (") and
                    long_version.endswith(")") and
                    len(long_version) > len(short_version) + 3))):
            raise RuntimeError("packaged CLI version does not match the release")
        graph = json.loads(run("--json", "init", "--no-login"))
        if graph["backend"] != "graph":
            raise RuntimeError("Graph is no longer the default backend")
        desktop = json.loads(run("--json", "init", "--backend", "desktop", "--no-login"))
        if desktop["backend"] != "desktop" or desktop["read_only"] is not True:
            raise RuntimeError("desktop profile contract changed")
        status = json.loads(run("--json", "auth", "status", "--offline"))
        if (status["signed_in"] is not None or status["session_verified"] is not False
                or status["connection"] is not None):
            raise RuntimeError("offline desktop status claims session or endpoint access")
        rejection = json.loads(run(
            "--json", "messages", "send", "--chat", "48:notes", "--body", "fixture",
            exit_code=2,
        ))
        if rejection["error"]["kind"] != "read_only":
            raise RuntimeError("desktop messaging bypassed the read-only default")
        unsupported = json.loads(run("--json", "whoami", exit_code=2))
        if unsupported["error"]["kind"] != "unsupported":
            raise RuntimeError("desktop identity did not return unsupported")
    print(json.dumps({"version": version, "packaged_cli_smoke": "passed"}))


if __name__ == "__main__":
    main()
