#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=.github/scripts/finalize-release.sh
source "${SCRIPT_DIR}/finalize-release.sh"

assert_eq() {
  local expected="$1" actual="$2" message="$3"
  if [[ "${expected}" != "${actual}" ]]; then
    echo "FAIL: ${message}: expected '${expected}', got '${actual}'" >&2
    exit 1
  fi
}

assert_dies() {
  local message="$1"
  shift
  if ( "$@" ) >/dev/null 2>&1; then
    echo "FAIL: ${message}: expected failure, got success" >&2
    exit 1
  fi
}

sync_body() {
  printf 'Automated final release sync.\n\nRelease tag: %s\nRelease branch: %s\n' "$1" "$2"
}

test_patch_release_kind_derived_from_tag() {
  PR_BODY="$(sync_body v0.7.2 release/0.7)"
  PR_BASE_REF="release/0.7"
  PR_HEAD_REF="sync/v0_7_2"
  parse_release_metadata
  assert_eq "patch" "${EFFECTIVE_RELEASE_KIND}" "patch tag (no Release kind field) derives patch"
}

test_minor_release_kind_derived_from_tag() {
  PR_BODY="$(sync_body v0.8.0 release/0.8)"
  PR_BASE_REF="main"
  PR_HEAD_REF="sync/v0_8_0"
  parse_release_metadata
  assert_eq "minor" "${EFFECTIVE_RELEASE_KIND}" "X.Y.0 tag (no Release kind field) derives minor"
}

test_major_release_kind_derived_from_tag() {
  PR_BODY="$(sync_body v1.0.0 release/1.0)"
  PR_BASE_REF="main"
  PR_HEAD_REF="sync/v1_0_0"
  parse_release_metadata
  assert_eq "major" "${EFFECTIVE_RELEASE_KIND}" "X.0.0 tag (no Release kind field) derives major, not minor"
}

test_major_release_rejects_release_branch_base() {
  PR_BODY="$(sync_body v1.0.0 release/1.0)"
  PR_BASE_REF="release/1.0"
  PR_HEAD_REF="sync/v1_0_0"
  assert_dies "major release PR targeting its own release branch instead of main must fail" parse_release_metadata
}

test_patch_release_kind_derived_from_tag
test_minor_release_kind_derived_from_tag
test_major_release_kind_derived_from_tag
test_major_release_rejects_release_branch_base

echo "finalize-release tests passed"
