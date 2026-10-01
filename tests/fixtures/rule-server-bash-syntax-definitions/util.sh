#!/usr/bin/env bash
# Banner helpers; loads lib.sh from the same directory.
. ./lib.sh

util_banner() {
  lib_log "== $1 =="
  util_line
}

util_line() {
  echo "----------"
}
