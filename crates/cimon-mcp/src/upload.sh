#!/bin/sh
# cimon upload - sends files or one-line values from this machine straight to the cimon
# server for one job, and prints the handles to pass to build/test/run as `uploads`.
# The bytes go from this machine to the server only - never through the Claude
# conversation. Served by the cimon server itself; the address below is that server's.
# The connect script keeps a copy in ~/.cimon/upload.sh.
#   sh ~/.cimon/upload.sh <name>=<source> ...
# <source>: a file path | env:<VARIABLE> (its value) | prompt (asks, without echo)
#   e.g. keystore=$HOME/keys/release.jks keystore-password=env:EB_KEYSTORE_PASSWORD key-password=prompt
set -eu
CIMON_URL='{{URL}}'

fail() { printf 'cimon upload: %s\n' "$1" >&2; exit 1; }

[ $# -gt 0 ] || fail "usage: upload.sh <name>=<file> | <name>=env:<VARIABLE> | <name>=prompt ..."
command -v curl >/dev/null 2>&1 || fail "curl is needed"
[ -f "$HOME/.cimon/header.sh" ] || fail "no ~/.cimon/header.sh - connect this machine first: curl -fsSL $CIMON_URL/connect.sh | sh"
# the token through the same header helper Claude Code uses; it goes to curl in a
# private config file, not on its command line
auth=$(sh "$HOME/.cimon/header.sh" | sed -n 's/.*"Authorization":"\([^"]*\)".*/\1/p')
[ -n "$auth" ] || fail "~/.cimon/header.sh printed no Authorization header"
cfg=$(umask 077 && mktemp)
# also on Ctrl-C at a prompt: echo back on (only if a prompt turned it off - there may
# be no terminal at all), the config file gone. set -e applies in a trap: nothing may fail
echo_off=""
trap '[ -z "$echo_off" ] || { stty echo < /dev/tty; } 2>/dev/null || true; rm -f "$cfg"' EXIT
trap 'exit 130' INT TERM
printf 'header = "Authorization: %s"\n' "$auth" > "$cfg"
unset auth

out=""
for item in "$@"; do
  name=${item%%=*}
  src=${item#*=}
  [ "$name" != "$item" ] && [ -n "$name" ] || fail "$item - expected <name>=<source>"
  case "$name" in
    *[!a-z0-9_-]*|[_-]*) fail "$name is not an upload name ([a-z0-9][a-z0-9_-]*, as the recipe writes it)" ;;
  esac
  case "$src" in
    prompt)
      kind=text
      # in a subshell: a failed redirection on a special builtin would end this shell
      (exec < /dev/tty) 2>/dev/null || fail "$name=prompt needs a terminal - use $name=env:<VARIABLE>"
      printf '%s: ' "$name" > /dev/tty
      echo_off=1
      { stty -echo < /dev/tty; } 2>/dev/null || true
      IFS= read -r value < /dev/tty || value=""
      { stty echo < /dev/tty; } 2>/dev/null || true
      echo_off=""
      printf '\n' > /dev/tty
      ;;
    env:*)
      kind=text
      value=$(printenv "${src#env:}" || true)
      ;;
    *)
      kind=file
      [ -f "$src" ] || fail "$name - no file $src"
      ;;
  esac
  url="$CIMON_URL/uploads?kind=$kind&label=$name"
  if [ "$kind" = text ]; then
    [ -n "$value" ] || fail "$name - the value is empty"
    # printf is a shell builtin: the value is on no command line
    resp=$(printf '%s' "$value" | curl -sS -K "$cfg" -H 'Content-Type: application/octet-stream' --data-binary @- -w '\n%{http_code}' "$url") || fail "$name - could not reach $CIMON_URL"
    value=""
  else
    resp=$(curl -sS -K "$cfg" -H 'Content-Type: application/octet-stream' --data-binary "@$src" -w '\n%{http_code}' "$url") || fail "$name - could not reach $CIMON_URL"
  fi
  status=$(printf '%s' "$resp" | tail -n 1)
  body=$(printf '%s' "$resp" | sed '$d')
  if [ "$status" != 200 ]; then
    msg=$(printf '%s' "$body" | sed -n 's/.*"message":"\([^"]*\)".*/\1/p')
    fail "$name - the server refused ($status): ${msg:-$body}"
  fi
  handle=$(printf '%s' "$body" | sed -n 's/.*"handle":"\(u_[A-Za-z0-9]*\)".*/\1/p')
  sha=$(printf '%s' "$body" | sed -n 's/.*"sha256":"\([0-9a-f]\{12\}\).*/\1/p')
  bytes=$(printf '%s' "$body" | sed -n 's/.*"bytes":\([0-9]*\).*/\1/p')
  until=$(printf '%s' "$body" | sed -n 's/.*"expires_at":"\([^"]*\)".*/\1/p')
  [ -n "$handle" ] || fail "$name - unexpected answer from the server: $body"
  fp=""
  [ -n "$sha" ] && fp=", $bytes bytes, sha256 $sha..."
  printf 'cimon upload: %s (%s%s) - usable until %s\n' "$name" "$kind" "$fp" "$until" >&2
  out="$out${out:+,}\"$name\":\"$handle\""
done
# pass this as `uploads` in the build/test/run call
printf '{%s}\n' "$out"
