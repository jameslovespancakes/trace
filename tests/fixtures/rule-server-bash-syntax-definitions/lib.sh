#!/usr/bin/env bash
# Text helpers shared by util.sh and main.sh.

lib_trim() {
  local value="$1"
  echo "${value## }"
}

lib_upper() {
  lib_trim "$1" | tr '[:lower:]' '[:upper:]'
}

lib_log() {
  printf '%s\n' "$(lib_upper "$1")"
}
