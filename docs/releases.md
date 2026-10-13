# Releases

Vership is the release control plane for `teams-cli`. GitHub Actions is the
execution plane for cross-platform builds, publication, provenance, and
downstream packaging.

## Create a release

Start from a clean, up-to-date `main` branch and run:

```sh
vership preflight
vership bump patch --prepare # or minor/major
# Review the candidate and obtain approval to push and publish.
vership release
tarry cmd --timeout 20m -- vership verify
```

Vership runs `make check`, updates the package version and changelog, creates
the Conventional Commit release commit. `--prepare` stops before tagging or
pushing. After approval, `vership release` publishes that same prepared version;
do not bump again. Its tag starts the release workflow.

Cargo and Python package versions are kept synchronized as one release unit.

## Alpha and beta releases

Use a prerelease such as `0.1.8-alpha.1` while desktop support is limited to
connection diagnostics. GitHub marks tags containing a prerelease suffix as
prereleases and leaves them out of the latest stable release selection. Maturin
normalizes the Python distribution version to PEP 440 (`0.1.8a1` for that example);
the CLI still reports its Cargo version.

Vership 0.5.26 has no prerelease bump option. Prepare the patch once with
`vership bump patch --prepare`, then apply the prerelease suffix to the unpublished
candidate's Cargo version, lockfile and changelog and amend its release commit,
including the `Vership-Release: v0.1.8-alpha.1` marker. This is a local workaround
for the unsupported version operation; continue with `vership release --dry-run`
and `vership release` for the reviewed candidate.

After the workflows finish, verify the Cargo/GitHub version and normalized Python
version separately:

```sh
vership verify 0.1.8-alpha.1 --targets tag,release,crates
vership verify 0.1.8a1 --targets pypi
```

The binary build jobs extract and smoke test their packaged CLI before uploading
it. Follow publication with installation tests on macOS, Windows and WSL. Desktop
diagnostics can be tested with isolated profiles; successful connection metadata
does not establish sign-in or message access.

## Failure policy

If a release fails before any release, artifact, package, checksum, or
attestation becomes public, delete and recreate the brief tag and retry the
same version. Once anything was published, preserve the tag and issue a patch
release for release-content corrections. Registry and downstream-packaging
jobs are separate recovery domains; retry only the failed job when possible.

## Dry runs

The Release workflow can be dispatched from `main` with `dry_run` enabled. It
uses the package version unless an explicit `version` is supplied, builds the
real artifacts, and exercises registry validation without publishing packages,
creating a GitHub release, generating attestations, or updating downstream
packaging.
