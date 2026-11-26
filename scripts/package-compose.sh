#!/usr/bin/env bash
# Builds the Compose install's release archive: the files of deploy/compose that git tracks, and
# no other, under the directory hd-compose-VERSION/, as OUTDIR/hd-compose-VERSION.tar.gz beside
# its SHA-256 checksum, hd-compose-VERSION.tar.gz.sha256. The release workflow attaches both to
# the GitHub release of a version tag (.github/workflows/release.yml); CI builds one on every
# pull request, tests it (scripts/test-package-compose.sh) and runs the install's test from it
# (scripts/test-compose.sh).
#
# VERSION is the tag without its `v`: 1.2.3, or 1.2.3-rc.1 for a pre-release.
#
# Usage:
#   bash scripts/package-compose.sh VERSION OUTDIR
#
# Environment: REPO (the git work tree whose deploy/compose is packaged, the one holding this
# script), SOURCE_DATE_EPOCH (the time given to the archive's files, the last commit's).
set -euo pipefail

usage() {
  echo "usage: $0 VERSION OUTDIR" >&2
  exit 2
}

refuse() {
  echo "package-compose: $*" >&2
  exit 1
}

[ $# -eq 2 ] && [ -n "$1" ] && [ -n "$2" ] || usage
version=$1
out=$2
repo=${REPO:-$(cd "$(dirname "$0")/.." && pwd)}

# Semantic Versioning 2 without build metadata, as for the chart (scripts/publish-chart.sh).
number='(0|[1-9][0-9]*)'
identifier="($number|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
[[ $version =~ ^$number\.$number\.$number(-$identifier(\.$identifier)*)?$ ]] \
  || refuse "$version is not a version (1.2.3, or 1.2.3-rc.1 for a pre-release)"

top="hd-compose-$version"
name="$top.tar.gz"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
dir="$stage/$top"
mkdir "$dir"

# The tracked files only: a .env or a secret written beside them is never packaged, and a file
# a glob would miss, such as .env.example or secrets/.gitignore, always is.
files=()
while IFS= read -r -d '' file; do
  files+=("${file#deploy/compose/}")
done < <(git -C "$repo" ls-files -z -- deploy/compose)
[ ${#files[@]} -gt 0 ] || refuse "git tracks no file in $repo/deploy/compose"
for file in "${files[@]}"; do
  mkdir -p "$dir/$(dirname "$file")"
  cp "$repo/deploy/compose/$file" "$dir/$file"
done

[ -d "$dir/secrets" ] || refuse "git tracks no file in $repo/deploy/compose/secrets"

# The images, named by the version in compose.yaml rather than read from .env, so that the next
# release's files run the next version with the .env an install keeps; .env.example shows it.
compose="$dir/compose.yaml"
image='^( +image: ghcr\.io/manustars/hd-(core|assistant)):'
variable='\$\{HD_VERSION:\?[^}]*\}$'
for component in core assistant; do
  [ "$(grep -cE "${image/(core|assistant)/$component}$variable" "$compose")" = 1 ] \
    || refuse "compose.yaml has no line, or more than one, naming the image of hd-$component" \
      "by HD_VERSION (image: ghcr.io/manustars/hd-$component:\${HD_VERSION:?...})"
done
sed -E -i "s#$image$variable#\\1:$version#" "$compose"
! grep -n HD_VERSION "$compose" || refuse "compose.yaml reads HD_VERSION elsewhere than the images"
env_example="$dir/.env.example"
[ "$(grep -c '^#HD_VERSION=' "$env_example")" = 1 ] \
  || refuse ".env.example has no line #HD_VERSION= to show the version on, or more than one"
sed -i "s/^#HD_VERSION=.*/#HD_VERSION=$version/" "$env_example"

# The repository's other files, which the archive does not hold, at the release's tag: the links
# of the pages that go up out of deploy/compose, the paths the pages give as code and those the
# other files name in their comments. Links between the archive's own files stay as they are.
blob="https://github.com/manustars/appticket/blob/v$version"
top_dirs=$(git -C "$repo" ls-files | sed -n 's|^\([^./][^/]*\)/.*|\1|p' | sort -u | paste -sd '|')
for file in "${files[@]}"; do
  path="$dir/$file"
  # How many ../ lead from the file's directory to deploy/compose, to deploy and to the root.
  up=$(printf '%s' "$file" | tr -cd / | sed 's|/|../|g')
  up_pattern=${up//./\\.}
  case $file in
    *.md)
      ! grep -nF "](../../$up../" "$path" \
        || refuse "$file links to a file outside the repository"
      sed -E -i \
        -e "s@\\]\\(\\.\\./\\.\\./$up_pattern@](${blob}/@g" \
        -e "s@\\]\\(\\.\\./$up_pattern@](${blob}/deploy/@g" \
        -e "s@(^|[^[])\`(($top_dirs)/[^\`]*)\`@\\1[\`\\2\`](${blob}/\\2)@g" \
        "$path"
      ;;
    *)
      sed -E -i \
        -e "s@(^|[^A-Za-z0-9_./-])(($top_dirs)/[A-Za-z0-9_./#-]*[A-Za-z0-9_-])@\\1${blob}/\\2@g" \
        "$path"
      ;;
  esac
done

# Every entry by name, the same on every machine, with the modes an install wants whatever the
# checkout gave, and secrets/ closed to other users of the host: tar gives each the mode in the
# archive, so the directory is added last, with its own (a directory's mode is set once its
# files are in).
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo" log -1 --format=%ct)}
(cd "$stage" && find "$top" ! -path "$top/secrets" | LC_ALL=C sort) > "$stage/entries"
entries() {
  tar -C "$stage" --owner=0 --group=0 --numeric-owner --mtime="@$epoch" --no-recursion "$@"
}
entries -cf "$stage/archive.tar" --mode=u=rwX,go=rX -T "$stage/entries"
entries -rf "$stage/archive.tar" --mode=u=rwx,go= "$top/secrets"
mkdir -p "$out"
gzip -9n < "$stage/archive.tar" > "$out/$name"
(cd "$out" && sha256sum "$name" > "$name.sha256")
echo "package-compose: $out/$name"
tar -tvzf "$out/$name"
