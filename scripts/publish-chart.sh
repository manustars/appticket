#!/usr/bin/env bash
# Publishes the Helm chart to an OCI registry with a release's version, after checking that the
# version agrees with the chart. The release workflow runs it on a version tag
# (.github/workflows/release.yml) and CI runs it against a throwaway registry
# (scripts/test-publish-chart.sh).
#
# VERSION is the tag without its `v`: 1.2.3, or 1.2.3-rc.1 for a pre-release. It agrees with the
# chart when Chart.yaml's `version` is the same or, for a pre-release, is the version the
# pre-release leads to (1.2.3-rc.1 can be cut while Chart.yaml says 1.2.3). Chart.yaml's
# `appVersion` must be the same as its `version`. The published chart has VERSION as both: its
# default image tags are its appVersion, and a release publishes the images under VERSION.
#
# Usage:
#   bash scripts/publish-chart.sh VERSION REGISTRY   check, package and push to REGISTRY
#                                                    (oci://ghcr.io/manustars/charts), which gives
#                                                    REGISTRY/hd:VERSION; log in first with
#                                                    `helm registry login`
#   bash scripts/publish-chart.sh --check VERSION    only check; needs no Helm
#
# --plain-http before VERSION pushes over HTTP, for a test registry.
# Environment: CHART (the chart's directory, deploy/chart).
set -euo pipefail

usage() {
  echo "usage: $0 [--plain-http] VERSION REGISTRY | --check VERSION" >&2
  exit 2
}

refuse() {
  echo "publish-chart: $*" >&2
  exit 1
}

check_only=
plain_http=()
case "${1:-}" in
  --check) check_only=1; shift ;;
  --plain-http) plain_http=(--plain-http); shift ;;
esac
version=${1:-}
registry=${2:-}
[ -n "$version" ] || usage
if [ -n "$check_only" ]; then
  [ $# -eq 1 ] || usage
else
  [ $# -eq 2 ] && [ -n "$registry" ] || usage
fi

chart=${CHART:-$(cd "$(dirname "$0")/.." && pwd)/deploy/chart}

# A field of Chart.yaml, without its quotes.
field() {
  sed -n "s/^$1: *//p" "$chart/Chart.yaml" | tr -d "\"'\r"
}

[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] \
  || refuse "$version is not a version (1.2.3, or 1.2.3-rc.1 for a pre-release)"

chart_version=$(field version)
chart_app_version=$(field appVersion)
[ -n "$chart_version" ] || refuse "no version in $chart/Chart.yaml"
[ "$chart_app_version" = "$chart_version" ] \
  || refuse "the chart's appVersion $chart_app_version differs from its version $chart_version"

# What a pre-release leads to: 1.2.3 for 1.2.3-rc.1. A final version is its own.
final=${version%%-*}
if [ "$version" != "$chart_version" ] && { [ "$final" = "$version" ] || [ "$final" != "$chart_version" ]; }; then
  refuse "version $version disagrees with the chart's version $chart_version: set version and" \
    "appVersion in Chart.yaml to ${final} before tagging"
fi
echo "publish-chart: version $version agrees with the chart's version $chart_version"
[ -z "$check_only" ] || exit 0

package=$(mktemp -d)
trap 'rm -rf "$package"' EXIT
helm package "$chart" -d "$package" --version "$version" --app-version "$version"
helm push "$package/hd-$version.tgz" "$registry" "${plain_http[@]}"
