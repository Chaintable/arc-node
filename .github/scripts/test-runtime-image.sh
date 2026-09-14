#!/usr/bin/env bash
set -euo pipefail

image_ref="${1:?usage: test-runtime-image.sh IMAGE BINARY}"
binary="${2:?usage: test-runtime-image.sh IMAGE BINARY}"

# Use the image's default user and isolated temporary files, never a node volume.
docker run --rm --network none --entrypoint /bin/bash "$image_ref" -euxc '
  [[ "$(id -u)" == 999 && "$(id -g)" == 999 ]]
  for command in bash sh cat touch tail rm mkdir mktemp; do
    command -v "$command"
  done
  test -s /etc/ssl/certs/ca-certificates.crt
  test_dir=$(mktemp -d)

  # The init script must delete only the discovery key, including on a rerun.
  mkdir "$test_dir/execution"
  touch "$test_dir/execution/discovery-secret" "$test_dir/execution/keep"
  for attempt in 1 2; do
    bash -euc '\''rm -rf -- "$1/execution/discovery-secret"'\'' bash "$test_dir"
    test ! -e "$test_dir/execution/discovery-secret"
    test -f "$test_dir/execution/keep"
  done

  printf "normal\n" > "$test_dir/runmode"
  runmode=$(cat "$test_dir/runmode")
  [[ X${runmode} == Xnormal ]]
  printf "prune\n" > "$test_dir/runmode"
  [[ "$(tail -n 1 "$test_dir/runmode")" == prune ]]
  runmode=$(cat "$test_dir/runmode")
  if [[ X${runmode} != Xnormal ]]; then
    if [[ X${runmode} == Xprune ]]; then
      touch "$test_dir/prune.done"
    fi
    touch "$test_dir/done.done"
    tail -f /dev/null &
    child=$!
    kill -TERM "$child"
    status=0
    wait "$child" || status=$?
    [[ "$status" == 143 ]]
  fi
  test -f "$test_dir/prune.done"
  test -f "$test_dir/done.done"
  /bin/sh -eu -c '\''test "$(cat "$1/runmode")" = prune'\'' sh "$test_dir"
  exec "/usr/local/bin/$1" --version
' bash "$binary"
