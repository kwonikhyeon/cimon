#!/bin/sh
# cimon connect - sets up this machine to use the cimon build service from Claude Code.
# Served by the cimon server itself; the address below is that server's.
#   curl -fsSL <server>/connect.sh | sh
# Asks for the invite code from `cimon invite`, stores the token in the keychain
# (macOS) or the keyring (Linux, secret-tool), writes the header helper and registers
# cimon with Claude Code.
set -eu
CIMON_URL='{{URL}}'

fail() { printf 'cimon connect: %s\n' "$1" >&2; exit 1; }

command -v claude >/dev/null 2>&1 || fail "Claude Code (claude) is not on PATH - install it first"
command -v curl >/dev/null 2>&1 || fail "curl is needed"
ver=$(claude --version 2>/dev/null | sed -n 's/^[^0-9]*\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' | head -n 1)
if [ -n "$ver" ]; then
  lowest=$(printf '%s\n2.1.304\n' "$ver" | sort -t. -k1,1n -k2,2n -k3,3n | head -n 1)
  [ "$lowest" = "2.1.304" ] || fail "Claude Code $ver is too old - run: claude update (MCP tasks need 2.1.304 or newer)"
fi

# piped into sh, stdin is this script: read the code from the terminal
# (CIMON_INVITE_CODE lets a provisioning script pass it without a prompt)
if [ -n "${CIMON_INVITE_CODE:-}" ]; then
  code=$CIMON_INVITE_CODE
else
  printf 'cimon invite code: ' > /dev/tty
  read -r code < /dev/tty
fi
code=$(printf '%s' "$code" | tr -cd 'A-Za-z0-9-')
device=$(hostname 2>/dev/null | tr -cd 'A-Za-z0-9._-')
resp=$(curl -fsS -X POST -H 'Content-Type: application/json' \
  --data "{\"code\":\"$code\",\"device\":\"$device\"}" "$CIMON_URL/connect/redeem") \
  || fail "the server refused the code - ask the admin for a new one (cimon invite)"
token=$(printf '%s' "$resp" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')
user=$(printf '%s' "$resp" | sed -n 's/.*"user":"\([A-Za-z0-9._-]*\)".*/\1/p')
[ -n "$token" ] || fail "unexpected answer from the server"

mkdir -p "$HOME/.cimon"
chmod 700 "$HOME/.cimon"
case "$(uname -s)" in
  Darwin)
    # -U replaces an earlier entry; the token is on this command line for a moment
    security add-generic-password -U -a "$USER" -s cimon -w "$token" >/dev/null \
      || fail "could not store the token in the keychain"
    get='security find-generic-password -a "$USER" -s cimon -w'
    ;;
  *)
    if command -v secret-tool >/dev/null 2>&1; then
      printf '%s' "$token" | secret-tool store --label=cimon service cimon || fail "could not store the token with secret-tool"
      get='secret-tool lookup service cimon'
    else
      (umask 077 && printf '%s' "$token" > "$HOME/.cimon/token")
      get='cat "$HOME/.cimon/token"'
      printf 'cimon connect: no keyring (secret-tool) - the token is kept in ~/.cimon/token (mode 600)\n'
    fi
    ;;
esac
cat > "$HOME/.cimon/header.sh" <<EOF
#!/bin/sh
printf '{"Authorization":"Bearer %s"}' "\$($get)"
EOF
chmod 700 "$HOME/.cimon/header.sh"

claude mcp remove --scope user cimon >/dev/null 2>&1 || true   # replace an earlier registration
claude mcp add-json --scope user cimon "{\"type\":\"http\",\"url\":\"$CIMON_URL/mcp\",\"headersHelper\":\"sh '$HOME/.cimon/header.sh'\"}" \
  || fail "claude mcp add-json failed"
# what Claude Code stored, and its own connection check through the header helper
got=$(claude mcp get cimon 2>&1 || true)
case "$got" in
  *"$CIMON_URL/mcp"*Connected*|*Connected*"$CIMON_URL/mcp"*) ;;
  *) fail "claude mcp get cimon does not show a connected server at $CIMON_URL/mcp: $got" ;;
esac

curl -fsS -H "Authorization: Bearer $token" "$CIMON_URL/healthz" | grep -q '"queued"' \
  || fail "registered, but the server did not accept the token"
unset token resp
printf '\ncimon connect: done - signed in as %s.\nStart Claude Code and ask, for example: cimon project list\n' "$user"
