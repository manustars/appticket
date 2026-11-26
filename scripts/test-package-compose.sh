#!/usr/bin/env bash
# Tests scripts/package-compose.sh, which a release runs: the archive it builds is unpacked and
# read as an install reads it. It holds the tracked files of deploy/compose and nothing else,
# under one directory; its images are those of its version, whatever .env says; its links lead
# to files inside it or to the repository's files at the release's tag; and what is not a
# version, or a Compose directory whose version cannot be written in, builds nothing.
#
# Usage:
#   bash scripts/test-package-compose.sh
#
# Needs git, GNU tar, gzip and sha256sum (as on Linux and in Git Bash).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
package="$root/scripts/package-compose.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# The files of the directory $1, separated by spaces, in order.
files_of() {
  find "$1" -mindepth 1 -maxdepth 1 -printf '%f\n' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//'
}

# The files of an archive of the version $1, as `tar -t` lists them, in order. Written out here
# rather than read from the repository: a file added to deploy/compose is added here too, on
# purpose.
expected_files() {
  local top="hd-compose-$1"
  printf '%s\n' "$top/" "$top/.env.example" "$top/Caddyfile" "$top/compose.yaml" \
    "$top/dumps.md" "$top/examples/" "$top/examples/Caddyfile" "$top/examples/nginx.conf" \
    "$top/examples/traefik.yaml" "$top/reverse-proxy.md" "$top/secrets/" \
    "$top/secrets/.gitignore" "$top/secrets/README.md"
}

# Builds the archive of the version $1 into the new directory $2 from the repository REPO (this
# one by default), checks that it is the only file there with its checksum, and unpacks it into
# $2/unpacked.
build() {
  local version=$1 out=$2 name="hd-compose-$1.tar.gz"
  bash "$package" "$version" "$out" > "$out.log" 2>&1 \
    || fail "the archive of $version was not built: $(cat "$out.log")"
  [ "$(files_of "$out")" = "$name $name.sha256" ] \
    || fail "$out holds '$(files_of "$out")', not the archive and its checksum"
  (cd "$out" && sha256sum -c "$name.sha256" > /dev/null) \
    || fail "the checksum does not match the archive: $(cat "$out/$name.sha256")"
  mkdir "$out/unpacked"
  tar -xzf "$out/$name" -C "$out/unpacked"
}

# A git repository in the new directory $1 whose index holds the tracked files of this
# repository's deploy/compose, as they are in the working tree, and nothing else.
repo_copy() {
  local file
  git init -q "$1"
  while IFS= read -r -d '' file; do
    mkdir -p "$1/$(dirname "$file")"
    cp "$root/$file" "$1/$file"
  done < <(git -C "$root" ls-files -z -- deploy/compose)
  git -C "$1" -c core.autocrlf=false add -- deploy/compose
}

# Builds as build does, from the copy of the repository $1, which has no commit to take the
# files' time from.
build_from() {
  local copy=$1
  shift
  REPO="$copy" SOURCE_DATE_EPOCH=1700000000 build "$@"
}

# Runs the command given after MESSAGE and OUT and checks that it refuses (status 1; 2 is a
# mistake in the arguments) saying MESSAGE, which it leaves in $output, and that it writes
# nothing into the directory OUT.
refused_with() {
  local message=$1 out=$2 status=0
  shift 2
  output=$("$@" 2>&1) || status=$?
  [ "$status" -eq 1 ] || fail "not refused (status $status): $* said: $output"
  grep -Fq -- "$message" <<< "$output" \
    || fail "refused without saying \"$message\": $* said: $output"
  if [ -e "$out" ] && [ -n "$(files_of "$out")" ]; then
    fail "refused, but wrote $(files_of "$out") into $out"
  fi
}

version=1.2.3-rc.1
top="hd-compose-$version"

# 1. What the archive holds.
build "$version" "$work/release"
listed=$(tar -tzf "$work/release/$top.tar.gz" | LC_ALL=C sort)
[ "$listed" = "$(expected_files "$version")" ] \
  || fail "the archive holds other files than expected:" \
    "$(diff <(expected_files "$version") - <<< "$listed")"
unpacked="$work/release/unpacked"
[ "$(files_of "$unpacked")" = "$top" ] || fail "the archive does not unpack into $top/ alone"
[ "$(files_of "$unpacked/$top/secrets")" = ".gitignore README.md" ] \
  || fail "the archive's secrets/ holds $(files_of "$unpacked/$top/secrets")"
# The mode as the archive holds it, which tar gives the directory on Linux.
secrets_mode=$(tar -tvzf "$work/release/$top.tar.gz" | grep -E " $top/secrets/\$" | cut -c1-10)
[ "$secrets_mode" = drwx------ ] || fail "the archive's secrets/ has the mode $secrets_mode"
echo "ok 1 - the archive of $version is $top.tar.gz, with its checksum, and unpacks into $top/" \
  "with the tracked files of deploy/compose, secrets/ holding README.md and .gitignore alone"

# 2. What git does not track is left out: an install's .env and secrets written into a copy of
# the repository's Compose directory, and a file no one added.
copy="$work/repo"
repo_copy "$copy"
echo "HD_PUBLIC_URL=https://planted.example.com" > "$copy/deploy/compose/.env"
for secret in db_password tls_key; do
  echo "planted-$secret" > "$copy/deploy/compose/secrets/$secret"
done
echo "# planted" > "$copy/deploy/compose/examples/untracked.conf"
build_from "$copy" "$version" "$work/planted"
listed=$(tar -tzf "$work/planted/$top.tar.gz" | LC_ALL=C sort)
[ "$listed" = "$(expected_files "$version")" ] \
  || fail "the archive holds what git does not track:" \
    "$(diff <(expected_files "$version") - <<< "$listed")"
! grep -rq planted "$work/planted/unpacked" || fail "the archive holds a planted file's content"
echo "ok 2 - a .env, secrets and a file that git does not track are left out of the archive"

# 3. The images are the version's, whatever .env says: compose.yaml names them by it and reads
# no HD_VERSION, so that the files of the next release run the next version with the same .env.
# The untouched .env.example shows the version, commented out.
install="$unpacked/$top"
images=$(grep -E '^ +image: ghcr\.io/manustars/' "$install/compose.yaml")
expected_images="    image: ghcr.io/manustars/hd-core:$version
    image: ghcr.io/manustars/hd-assistant:$version"
[ "$images" = "$expected_images" ] || fail "the archive's compose.yaml names the images $images"
! grep -n HD_VERSION "$install/compose.yaml" \
  || fail "the archive's compose.yaml reads HD_VERSION"
[ "$(grep -E '^#? ?HD_VERSION=' "$install/.env.example")" = "#HD_VERSION=$version" ] \
  || fail "the archive's .env.example has $(grep -E 'HD_VERSION=' "$install/.env.example")"
echo "ok 3 - the archive's compose.yaml names the images of $version and reads no HD_VERSION," \
  "and its .env.example shows #HD_VERSION=$version"

# 4. The links work outside the repository: a link to a file of the archive leads to it, and
# what the repository holds beside deploy/compose is linked and named at the release's tag.
blob="https://github.com/manustars/appticket/blob/v$version"
# What the archive links to among its own files, as FILE TARGET lines.
while IFS= read -r -d '' md; do
  grep -oE '\]\([^)]*\)' "$md" | sed -E 's/^\]\(//; s/\)$//' | sed "s|^|${md#"$install/"} |"
done < <(find "$install" -name '*.md' -print0) > "$work/links"
[ -s "$work/links" ] || fail "the archive's pages link to nothing: the check would find nothing"
local_links=0
while read -r md target; do
  case $target in
    https://*) continue ;;
    "#"*) continue ;;
  esac
  path=${target%%#*}
  [ -f "$install/$(dirname "$md")/$path" ] \
    || fail "$md links to $target, which is not in the archive"
  local_links=$((local_links + 1))
done < "$work/links"
# Every address of the repository's files is at the tag and names a file the repository tracks.
grep -rhoE 'https://github\.com/manustars/appticket/[^ )>`"]*' "$install" > "$work/urls" || true
[ -s "$work/urls" ] || fail "the archive gives no address of the repository's files"
while read -r url; do
  path=${url#"$blob/"}
  [ "$path" != "$url" ] || fail "the archive gives $url, not at $blob"
  path=${path%%#*}
  # Without the punctuation of the sentence it ends.
  path=${path%[.,:;]}
  git -C "$root" ls-files --error-unmatch -- "$path" > /dev/null 2>&1 \
    || fail "the archive gives $url, a file that the repository does not track"
done < "$work/urls"
# No path of the repository is left beside them, in a comment or in a page's text, other than as
# the text of a link to its address.
top_dirs=$(git -C "$root" ls-files | sed -n 's|^\([^./][^/]*\)/.*|\1|p' | sort -u | paste -sd '|')
sed -E 's/\[[^]]*\]\(https:[^)]*\)//g; s#https://[^ )>`"]*##g' "$install"/.env.example \
  "$install"/*.yaml "$install"/*.md "$install"/Caddyfile "$install"/examples/* \
  "$install"/secrets/README.md > "$work/text"
! grep -nE "(^|[^A-Za-z0-9_./-])($top_dirs)/" "$work/text" \
  || fail "the archive names files of the repository by their paths, which lead nowhere outside it"
grep -qF "]($blob/docs/external-database.md#secrets-from-files)" "$install/secrets/README.md" \
  || fail "secrets/README.md does not link to the secrets from files at $blob"
grep -qF "](../reverse-proxy.md)" "$install/secrets/README.md" \
  || fail "secrets/README.md no longer links to ../reverse-proxy.md, in the archive"
echo "ok 4 - the archive's $local_links links to its own files lead to them, and its" \
  "$(wc -l < "$work/urls") addresses of the repository's files are at v$version and lead to" \
  "tracked files"

# 5. What is not a version (the release workflow runs on every v* tag) builds nothing.
refused=
for not_a_version in 1.2.3_final 1.2.3- 1.2.3-rc..1 1.2.3-01 01.2.3 v1.2.3 1.2.3+build; do
  refused_with "$not_a_version is not a version" "$work/not-a-version" \
    bash "$package" "$not_a_version" "$work/not-a-version"
  refused="$refused $not_a_version"
done
echo "ok 5 - what is not a version is refused and builds nothing:$refused"

# 6. A Compose directory whose version cannot be written in builds nothing: compose.yaml naming
# an image otherwise than by HD_VERSION, or reading HD_VERSION elsewhere, or .env.example
# without its line #HD_VERSION=.
broken() {
  local name=$1 file=$2 expression=$3
  repo_copy "$work/$name"
  sed -i "$expression" "$work/$name/deploy/compose/$file"
}
# shellcheck disable=SC2016 # the variables are compose.yaml's
broken fixed-tag compose.yaml 's/hd-assistant:\${HD_VERSION:?[^}]*}/hd-assistant:1.0.0/'
# shellcheck disable=SC2016
broken more-reads compose.yaml 's/^  db:$/  db:\n    labels: [version=${HD_VERSION}]/'
broken no-line .env.example '/^#HD_VERSION=/d'
broken set-line .env.example 's/^#HD_VERSION=/HD_VERSION=/'
broken escaping dumps.md 's|(\.\./\.\./docs/kubernetes|(../../../docs/kubernetes|'
REPO="$work/fixed-tag" refused_with "no line, or more than one, naming the image of hd-assistant" \
  "$work/out-fixed-tag" bash "$package" "$version" "$work/out-fixed-tag"
REPO="$work/more-reads" refused_with "compose.yaml reads HD_VERSION elsewhere than the images" \
  "$work/out-more-reads" bash "$package" "$version" "$work/out-more-reads"
for name in no-line set-line; do
  REPO="$work/$name" refused_with ".env.example has no line #HD_VERSION=" "$work/out-$name" \
    bash "$package" "$version" "$work/out-$name"
done
REPO="$work/escaping" refused_with "dumps.md links to a file outside the repository" \
  "$work/out-escaping" bash "$package" "$version" "$work/out-escaping"
echo "ok 6 - an image not named by HD_VERSION, HD_VERSION read elsewhere, .env.example without" \
  "#HD_VERSION= and a link out of the repository build nothing"

# 7. A mistake in the arguments.
for arguments in "" "$version" "$version $work/a $work/b"; do
  status=0
  # shellcheck disable=SC2086 # the arguments are split on purpose
  bash "$package" $arguments > /dev/null 2>&1 || status=$?
  [ "$status" -eq 2 ] || fail "the arguments '$arguments' give the status $status, not 2"
done
echo "ok 7 - no version, no output directory or a third argument is a mistake (status 2)"

echo "all cases passed"
