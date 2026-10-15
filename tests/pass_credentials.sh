#!/usr/bin/env bash
# Requires pass and GnuPG. All keys, agents and secrets are isolated test data.
set -euo pipefail
command -v pass >/dev/null
command -v gpg >/dev/null
credential_test_dir=$(mktemp -d)
export GNUPGHOME="$credential_test_dir/gnupg"
mkdir -m 700 "$GNUPGHOME"
cleanup() {
    gpgconf --kill gpg-agent >/dev/null 2>&1 || true
    rm -rf "$credential_test_dir"
}
trap cleanup EXIT

gpg --batch --pinentry-mode loopback --passphrase '' \
    --quick-generate-key 'teams-cli synthetic credential test' future-default default never \
    >/dev/null 2>&1
export TEAMS_TEST_PASS_STORE="$credential_test_dir/store"
mkdir -m 700 "$TEAMS_TEST_PASS_STORE"
gpg --batch --with-colons --list-keys 2>/dev/null \
    | awk -F: '$1 == "fpr" { print $10; exit }' > "$TEAMS_TEST_PASS_STORE/.gpg-id"
cargo test --locked --lib \
    auth::pass::tests::real_pass_gpg_round_trip_and_failed_replacement_preserve_encrypted_credentials \
    -- --ignored --exact
