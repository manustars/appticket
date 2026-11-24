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
#
# Environment: CHART (the chart's directory, deploy/chart), as for the script it tests.
set -euo pipefail

registry=${1:-127.0.0.1:55466}
root=$(cd "$(dirname "$0")/.." && pwd)
publish="$root/scripts/publish-chart.sh"
chart=${CHART:-$root/deploy/chart}
export CHART="$chart"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# A field of a chart's Chart.yaml, read from standard input, without its quotes or a comment.
field() {
  sed -n "s/^$1: *//p" | sed 's/[[:space:]]*#.*$//' | tr -d "\"'\r"
}

# A copy of the chart, in the directory $1, whose version and appVersion are $2; $3 is what
# follows each of them on its line.
chart_at() {
  cp -r "$chart" "$1"
  sed -i -e "s/^version:.*/version: $2${3:-}/" -e "s/^appVersion:.*/appVersion: \"$2\"${3:-}/" \
    "$1/Chart.yaml"
}

# The versions the registry holds for the chart.
published() {
  curl -sS "http://$registry/v2/charts/hd/tags/list"
}

# The registry lists the versions pushed in cases 1 and 2, and not the version $1.
not_published() {
  local tags pushed
  tags=$(published)
  for pushed in "$chart_version" "$prerelease"; do
    grep -Fq "\"$pushed\"" <<< "$tags" || fail "the registry does not list $pushed: $tags"
  done
  ! grep -Fq "\"$1\"" <<< "$tags" || fail "version $1 was published: $tags"
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

# Runs the script with the arguments given and checks that it refuses them (status 1; 2 is a
# mistake in the arguments) with a message containing $1, which it leaves in $output.
refused_with() {
  local message=$1 status=0
  shift
  output=$("$@" 2>&1) || status=$?
  [ "$status" -eq 1 ] || fail "not refused (status $status): $* said: $output"
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
# The final version that the chart's version is or leads to: the chart may hold a pre-release.
final=${chart_version%%-*}

# 1. The chart's own version.
bash "$publish" --check "$chart_version"
bash "$publish" --plain-http "$chart_version" "oci://$registry/charts"
pulled_back_is "$chart_version"
echo "ok 1 - version $chart_version, which agrees with the chart, is published, pulled back and rendered"

# 2. A pre-release of the version of a chart that holds a final version: the chart itself, or a
# copy at the final version when the chart holds a pre-release.
prerelease="$final-rc.1"
final_chart=$chart
if [ "$chart_version" != "$final" ]; then
  final_chart="$work/final"
  chart_at "$final_chart" "$final"
fi
CHART="$final_chart" bash "$publish" --plain-http "$prerelease" "oci://$registry/charts"
pulled_back_is "$prerelease"
echo "ok 2 - pre-release $prerelease of a chart at $final is published, pulled back and rendered as $prerelease"

# 3. A version that disagrees with the chart's: the next major one, which no bump of the chart
# makes agree.
other="$((${final%%.*} + 1)).${final#*.}"
message="version $other disagrees with the chart's version $chart_version"
refused_with "$message" bash "$publish" --check "$other"
refused_with "$message" bash "$publish" --plain-http "$other" "oci://$registry/charts"
not_published "$other"
echo "ok 3 - version $other, which disagrees with the chart, is refused and not published: $output"

# 4. A pre-release of another version.
refused_with "version $other-rc.1 disagrees with the chart's version $chart_version" \
  bash "$publish" --plain-http "$other-rc.1" "oci://$registry/charts"
not_published "$other-rc.1"
echo "ok 4 - pre-release $other-rc.1 of another version is refused and not published: $output"

# 5. A chart whose appVersion is not its version.
cp -r "$chart" "$work/chart"
sed -i "s/^appVersion:.*/appVersion: \"$other\"/" "$work/chart/Chart.yaml"
CHART="$work/chart" refused_with "the chart's appVersion $other differs from its version $chart_version" \
  bash "$publish" --check "$chart_version"
echo "ok 5 - a chart whose appVersion differs from its version is refused: $output"

# 6. A tag that is not a version (the release workflow runs on every v* tag), or that Helm would
# refuse or rewrite when it packages the chart, which is after the images are published.
refused=
for not_a_version in "${final}_final" "$final-" "$final-.." "$final-rc..1" "$final-01" "0$final" \
  "$final+build"; do
  refused_with "$not_a_version is not a version" bash "$publish" --check "$not_a_version"
  refused="$refused $not_a_version"
done
echo "ok 6 - what is not a version is refused:$refused"

# 7. A chart that holds a pre-release agrees with that pre-release only.
chart_at "$work/prerelease" "$final-rc.2"
CHART="$work/prerelease" bash "$publish" --check "$final-rc.2"
for disagreeing in "$final" "$final-rc.1"; do
  CHART="$work/prerelease" refused_with \
    "version $disagreeing disagrees with the chart's version $final-rc.2" \
    bash "$publish" --check "$disagreeing"
done
echo "ok 7 - a chart at $final-rc.2 agrees with $final-rc.2 and refuses $final and $final-rc.1"

# 8. A comment after the version in Chart.yaml is not part of the version.
chart_at "$work/commented" "$final" " # a comment"
CHART="$work/commented" bash "$publish" --check "$final"
echo "ok 8 - a chart whose version is followed by a comment agrees with $final"

echo "all cases passed"
