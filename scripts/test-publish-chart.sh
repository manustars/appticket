#!/usr/bin/env bash
# Tests scripts/publish-chart.sh, which a release runs, against a throwaway OCI registry: what it
# publishes is pulled back from the registry and rendered, and a version that disagrees with the
# chart publishes nothing.
#
# Usage:
#   bash scripts/test-publish-chart.sh [HOST:PORT]
#
# HOST:PORT is a registry that speaks plain HTTP and can be thrown away (127.0.0.1:55466 by
# default). Start one with
#   podman run -d --name hd-registry -p 127.0.0.1:55466:5000 docker.io/library/registry:3.0.0
# and remove it afterwards with `podman rm -f -v hd-registry`.
set -euo pipefail

registry=${1:-127.0.0.1:55466}
root=$(cd "$(dirname "$0")/.." && pwd)
publish="$root/scripts/publish-chart.sh"
chart="$root/deploy/chart"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# A field of a chart's Chart.yaml, read from standard input, without its quotes.
field() {
  sed -n "s/^$1: *//p" | tr -d "\"'\r"
}

# The versions the registry holds for the chart.
published() {
  curl -sS "http://$registry/v2/charts/hd/tags/list"
}

# Pulls the chart's version $1 back from the registry and checks that it is that version, with
# that appVersion, and that it renders the core's image of that version.
pulled_back_is() {
  local version=$1 dir="$work/pulled-$1" shown
  mkdir "$dir"
  helm pull "oci://$registry/charts/hd" --version "$version" --plain-http -d "$dir" > /dev/null
  shown=$(helm show chart "$dir/hd-$version.tgz")
  [ "$(field version <<< "$shown")" = "$version" ] \
    || fail "the pulled chart's version is not $version: $shown"
  [ "$(field appVersion <<< "$shown")" = "$version" ] \
    || fail "the pulled chart's appVersion is not $version: $shown"
  helm template hd "$dir/hd-$version.tgz" -s templates/core.yaml > "$dir/core.yaml"
  grep -Eq "^ +image: \"?ghcr\.io/manustars/hd-core:$version\"?\$" "$dir/core.yaml" \
    || fail "the pulled chart does not render the core's image $version: $(grep 'image:' "$dir/core.yaml")"
}

# Runs the script with the arguments given and checks that it refuses with a message containing
# $1, which it leaves in $output.
refused_with() {
  local message=$1 status=0
  shift
  output=$("$@" 2>&1) || status=$?
  [ "$status" -ne 0 ] || fail "accepted: $* said: $output"
  grep -Fq -- "$message" <<< "$output" || fail "refused without saying \"$message\": $* said: $output"
}

helm version --short

for _ in $(seq 30); do
  if curl -sf "http://$registry/v2/" > /dev/null; then
    break
  fi
  sleep 1
done
curl -sf "http://$registry/v2/" > /dev/null || fail "no registry answers at $registry"

chart_version=$(field version < "$chart/Chart.yaml")
[ -n "$chart_version" ] || fail "no version in $chart/Chart.yaml"

# 1. The chart's own version.
bash "$publish" --check "$chart_version"
bash "$publish" --plain-http "$chart_version" "oci://$registry/charts"
pulled_back_is "$chart_version"
echo "ok 1 - version $chart_version, which agrees with the chart, is published, pulled back and rendered"

# 2. A pre-release of the chart's version.
prerelease="$chart_version-rc.1"
bash "$publish" --plain-http "$prerelease" "oci://$registry/charts"
pulled_back_is "$prerelease"
echo "ok 2 - pre-release $prerelease of the chart's version is published, pulled back and rendered as $prerelease"

# 3. A version that disagrees with the chart's: the next major one, which no bump of the chart
# makes agree.
other="$((${chart_version%%.*} + 1)).${chart_version#*.}"
message="version $other disagrees with the chart's version $chart_version"
refused_with "$message" bash "$publish" --check "$other"
refused_with "$message" bash "$publish" --plain-http "$other" "oci://$registry/charts"
tags=$(published)
grep -Fq "\"$chart_version\"" <<< "$tags" || fail "the registry does not list its versions: $tags"
! grep -Fq "\"$other\"" <<< "$tags" || fail "version $other was published: $tags"
echo "ok 3 - version $other, which disagrees with the chart, is refused and not published: $output"

# 4. A pre-release of another version.
refused_with "version $other-rc.1 disagrees with the chart's version $chart_version" \
  bash "$publish" --plain-http "$other-rc.1" "oci://$registry/charts"
! grep -Fq "\"$other-rc.1\"" <<< "$(published)" || fail "version $other-rc.1 was published"
echo "ok 4 - pre-release $other-rc.1 of another version is refused and not published: $output"

# 5. A chart whose appVersion is not its version.
cp -r "$chart" "$work/chart"
sed -i "s/^appVersion:.*/appVersion: \"$other\"/" "$work/chart/Chart.yaml"
CHART="$work/chart" refused_with "the chart's appVersion $other differs from its version $chart_version" \
  bash "$publish" --check "$chart_version"
echo "ok 5 - a chart whose appVersion differs from its version is refused: $output"

# 6. A tag that is not a version (the release workflow runs on every v* tag).
refused_with "is not a version" bash "$publish" --check "${chart_version}_final"
echo "ok 6 - ${chart_version}_final, which is not a version, is refused: $output"

echo "all cases passed"
