#!/usr/bin/env bash
set -euo pipefail

image_ref="${1:?usage: test-runtime-image.sh IMAGE BINARY}"
binary="${2:?usage: test-runtime-image.sh IMAGE BINARY}"

# Match kubectl exec's direct execution: a Bash builtin is not sufficient.
docker run --rm --network none --entrypoint kill "$image_ref" -l TERM

# Use the image's default user and isolated temporary files, never a node volume.
docker run --rm --network none --entrypoint /bin/bash "$image_ref" -euxc '
  [[ "$(id -u)" == 999 && "$(id -g)" == 999 ]]
  for command in bash sh cat touch tail rm mkdir mktemp ls sleep kill; do
    type -P "$command"
  done
  test -s /etc/ssl/certs/ca-certificates.crt
  test ! -w /
  test -w /tmp
  test ! -e /tmp/prune.done
  test ! -e /tmp/done.done
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
      touch /tmp/prune.done
    fi
    touch /tmp/done.done
    tail -f /dev/null &
    child=$!
    env kill -TERM "$child"
    status=0
    wait "$child" || status=$?
    [[ "$status" == 143 ]]
  fi
  test -f /tmp/prune.done
  test -f /tmp/done.done
  /bin/sh -eu -c '\''test "$(cat "$1/runmode")" = prune'\'' sh "$test_dir"
  exec "/usr/local/bin/$1" --version
' bash "$binary"
