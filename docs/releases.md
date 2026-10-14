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

Use a prerelease while desktop support relies on experimental Teams internals.
The `0.1.11-beta.1` candidate promotes the desktop implementation validated in
`0.1.10-alpha.1`, with clearer setup, installation and support boundaries. It
supports cached chat reads and opt-in plain-text sends through Teams on macOS,
Windows and WSL. The Windows helper speaks protocol 3. Personal Microsoft accounts
use the desktop backend; Graph remains limited to work/school accounts.

GitHub marks tags containing a prerelease suffix as prereleases and leaves them
out of the latest stable release selection. Maturin normalizes the Python
package version to PEP 440 (`0.1.11b1` for this candidate); the CLI still reports
its Cargo version, `0.1.11-beta.1`.

Vership 0.5.26 has no prerelease bump option. Prepare the patch once with
`vership bump patch --prepare`, then apply the prerelease suffix to the unpublished
candidate's Cargo version, lockfile and changelog and amend its release commit,
including the `Vership-Release: v0.1.11-beta.1` marker. This is a local workaround
for the unsupported version operation. Run `make check` on the final candidate,
then continue with `vership release --dry-run` and, after approval,
`vership release`. Do not run another bump for a prepared candidate.

After the workflows finish, verify the Cargo/GitHub version and normalized Python
version separately:

```sh
vership verify 0.1.11-beta.1 --targets tag,release,crates
vership verify 0.1.11b1 --targets pypi
```

The binary build jobs extract and smoke test their packaged CLI before uploading
it. Follow publication with installation tests on macOS, Windows and WSL,
including the matching Windows helper's automatic download under WSL. Desktop
diagnostics can be tested with isolated profiles; successful connection metadata
does not establish sign-in or message access.

### Beta validation scope

The following runtime coverage was collected against the published
`0.1.10-alpha.1` desktop implementation before beta preparation:

| Account | Apple Silicon macOS | Native Windows | WSL2 |
| --- | --- | --- | --- |
| Personal | Instrumented reads, send and exact readback | Instrumented reads, send and exact readback | Instrumented reads, send and exact readback |
| Work/school | Instrumented cache reads | No separate work/school send validation | User-reported reads, send and readback |

The work/school report did not capture the exact CLI or Teams version. Keep it
separate from instrumented evidence. The user also reported Windows working;
individual native Windows TUI interactions were not instrumented.

Instrumented checks also covered cache pagination, read-only refusal, explicit
account-surface selection and ambiguity refusal, malformed cached text and
sentinel timestamps, macOS Teams restart recovery, WSL helper replacement while
the TUI stayed open, and normal shutdown with debugging closed. A controlled
helper fixture verified one send attempt and an `unconfirmed` error after a lost
response; no real Teams send was deliberately interrupted. Each real test send
was issued once and read back by its exact message ID.

Beta does not imply stable compatibility with Teams internals. Multi-day daily
use, actual account switching, macOS work/school sending, live Intel Mac coverage,
and broader Teams-version and managed-policy coverage remain to be validated.
The cache contains only synced history; only existing chats and plain-text sends
are supported. Missing cached profiles leave names or viewer identity unknown
without breaking reads. Multiple accounts within one selected surface are
refused. Keep these limits visible when assessing readiness for a stable release.

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
