#!/usr/bin/env bash
# Entry point: sources the helpers and runs two steps.
source ./lib.sh
source ./util.sh

main_step() {
  lib_trim "step $1"
  printf 'step %s\n' "$1"
}

main_run() {
  util_banner "start"
  lib_log "running"
  main_step 1
  main_step 2
  echo "done"
}

main_run "$@"
