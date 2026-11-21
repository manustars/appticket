#!/usr/bin/env bash
# Keeps the repository free of data that does not belong in it.
#
# Three checks, on contents and on paths:
#   1. denylist: extended regexes, one per line, case-insensitive, read from the file named by
#      $SCAN_DENYLIST or, if unset, by `git config scan.denylist`. The list is private and is
#      never committed. If neither is set, or the file is missing, the scan FAILS; the value
#      `none` skips only this check on purpose (CI, contributors without the list).
#   2. IPv4 addresses: only documentation ranges (RFC 5737), loopback, masks and the base
#      address of the private and special ranges are allowed.
#   3. e-mail addresses: only reserved domains (RFC 2606/6761), GitHub noreply and the
#      Co-Authored-By address of the coding assistant.
#
# Usage:
#   scripts/scan-clean.sh                 tracked and untracked, not ignored, files
#   scripts/scan-clean.sh --staged        the staged version of the changed files (pre-commit)
#   scripts/scan-clean.sh --message FILE  a commit message (commit-msg)
#   scripts/scan-clean.sh --commit SHA    a commit: message, added lines, paths, author (pre-push)
#   scripts/scan-clean.sh --text STRING   a single string, e.g. a branch or tag name (pre-push)
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"

mode=tree
arg=
case "${1:-}" in
  --staged) mode=staged ;;
  --message|--commit|--text) mode=${1#--}; arg=${2:?$1 needs a value} ;;
  '') ;;
  *) echo "scan-clean: unknown option $1" >&2; exit 2 ;;
esac

denylist=${SCAN_DENYLIST:-$(git config --get scan.denylist || true)}
if [ -z "$denylist" ]; then
  echo "scan-clean: no denylist configured" >&2
  echo "scan-clean: run 'git config scan.denylist <path>' or set SCAN_DENYLIST (or 'none' to skip that check)" >&2
  exit 2
fi
patterns=
tmp=$(mktemp)
trap 'rm -f "$tmp" ${patterns:+"$patterns"}' EXIT
if [ "$denylist" != none ]; then
  if [ ! -f "$denylist" ]; then
    echo "scan-clean: denylist not found at $denylist" >&2
    exit 2
  fi
  patterns=$(mktemp)
  grep -vE '^[[:space:]]*(#|$)' "$denylist" > "$patterns" || true
  if [ ! -s "$patterns" ]; then
    echo "scan-clean: denylist $denylist has no patterns" >&2
    exit 2
  fi
fi

ip_re='\b([0-9]{1,3}\.){3}[0-9]{1,3}\b'
ip_ok='^(0\.0\.0\.0|127\.[0-9.]+|255\.[0-9.]+|192\.0\.2\.[0-9]+|198\.51\.100\.[0-9]+|203\.0\.113\.[0-9]+|10\.0\.0\.0|172\.16\.0\.0|192\.168\.0\.0|169\.254\.0\.0|100\.64\.0\.0|224\.0\.0\.0|240\.0\.0\.0)$'
mail_re='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
mail_ok='@([A-Za-z0-9-]+\.)*(example\.(com|org|net)|example|test|invalid|localhost|users\.noreply\.github\.com|anthropic\.com)$'

found=0
report() { echo "$1: $2" >&2; found=1; }

# Reports every violation in the file $2, labelled with $1. Runs in this shell (nothing is
# piped into it), so that report() can set $found.
check_file() {
  local label=$1 file=$2 hits
  if [ -n "$patterns" ]; then
    hits=$(grep -niE -f "$patterns" "$file" || true)
    [ -n "$hits" ] && while IFS= read -r line; do report "$label" "denylist: $line"; done <<< "$hits"
  fi
  hits=$( (grep -noE "$ip_re" "$file" || true) | while IFS=: read -r n ip; do
    printf '%s\n' "$ip" | grep -qE "$ip_ok" || echo "$n:$ip"
  done)
  [ -n "$hits" ] && while IFS= read -r line; do report "$label" "ip not allowed: $line"; done <<< "$hits"
  hits=$( (grep -noE "$mail_re" "$file" || true) | while IFS= read -r m; do
    printf '%s\n' "${m#*:}" | grep -qiE "$mail_ok" || echo "$m"
  done)
  [ -n "$hits" ] && while IFS= read -r line; do report "$label" "e-mail not allowed: $line"; done <<< "$hits"
  return 0
}

# Checks the file $2 unless it is binary; labelled with $1.
check_text_file() {
  grep -qI . "$2" || return 0
  check_file "$1" "$2"
}

git_q() { git -c core.quotepath=off "$@"; }

case "$mode" in
  message)
    grep -v '^#' "$arg" > "$tmp" || true
    check_file "commit message" "$tmp"
    ;;
  text)
    printf '%s\n' "$arg" > "$tmp"
    check_file "name" "$tmp"
    ;;
  commit)
    c=$(git rev-parse --verify "$arg^{commit}")
    git log -1 --format=%B "$c" > "$tmp"
    check_file "commit $c message" "$tmp"
    git_q diff-tree --root --no-commit-id --name-only -r "$c" > "$tmp"
    check_file "commit $c paths" "$tmp"
    # Added lines only: what this commit puts into history.
    git_q diff-tree --root --no-commit-id -p -U0 --no-color --no-renames -r "$c" \
      | { grep -E '^\+' || true; } | { grep -vE '^\+\+\+ ' || true; } > "$tmp"
    check_file "commit $c content" "$tmp"
    for who in "$(git log -1 --format=%ae "$c")" "$(git log -1 --format=%ce "$c")"; do
      case "$who" in
        *@users.noreply.github.com) ;;
        *) report "commit $c" "author or committer is not a GitHub noreply address: $who" ;;
      esac
    done
    ;;
  tree|staged)
    if [ "$mode" = staged ]; then
      files=$(git_q diff --cached --name-only --diff-filter=ACMR)
    else
      files=$(git_q ls-files --cached --others --exclude-standard --deduplicate)
    fi
    printf '%s\n' "$files" > "$tmp"
    check_file "file names" "$tmp"
    while IFS= read -r f; do
      [ -z "$f" ] && continue
      if [ "$mode" = staged ]; then
        git show ":$f" > "$tmp"
        check_text_file "$f" "$tmp"
      elif [ -f "$f" ] && [ -r "$f" ]; then
        check_text_file "$f" "$f"
      else
        report "$f" "listed by git but not readable (deleted? stage the deletion)"
      fi
    done <<< "$files"
    ;;
esac

if [ "$found" -ne 0 ]; then
  echo "scan-clean: FAILED" >&2
  exit 1
fi
echo "scan-clean: ok ($mode)"
