#!/bin/sh
# Mirror a release's three packages to the R2 bucket behind dl.ducklocal.app,
# which ducklocal.app links to: GitHub's download host is slow or unreachable
# for many visitors in mainland China.
#
#   scripts/mirror-to-r2.sh v0.1.2 [package ...]
#
# With packages given (the release workflow passes the files it just
# published), those are uploaded; without, the three are fetched from the
# GitHub release of that tag first, which backfills an older release.
#
# Each package goes to <tag>/<name>, and — unless the tag is a prerelease
# (it holds a "-") — to latest/<name> as well. Versioned copies never change;
# latest/ is revalidated every few minutes so a new release shows up soon.
#
# Needs CLOUDFLARE_API_TOKEN (R2 Storage:Edit) and CLOUDFLARE_ACCOUNT_ID, or a
# `wrangler login`. R2_BUCKET overrides the bucket name.
set -eu

tag=${1:?usage: mirror-to-r2.sh <tag> [package ...]}
shift
bucket=${R2_BUCKET:-ducklocalapp}
names="ducklocal-macos-arm64.dmg ducklocal-linux-x86_64.tar.gz ducklocal-windows-x86_64.zip"

if [ $# -eq 0 ]; then
  dir=$(mktemp -d)
  trap 'rm -rf "$dir"' EXIT
  for name in $names; do
    echo "fetching $name from the GitHub release $tag"
    curl -fsSL -o "$dir/$name" \
      "https://github.com/JetSquirrel/DuckLocal/releases/download/$tag/$name"
  done
  set -- "$dir"/*
fi

put() {
  npx --yes wrangler@4 r2 object put "$bucket/$1" --remote \
    --file "$2" --content-type "$3" --cache-control "$4"
}

for file in "$@"; do
  name=$(basename "$file")
  case "$name" in
    *.dmg)    type=application/x-apple-diskimage ;;
    *.tar.gz) type=application/gzip ;;
    *.zip)    type=application/zip ;;
    *) echo "not a package: $file" >&2; exit 1 ;;
  esac
  put "$tag/$name" "$file" "$type" "public, max-age=31536000, immutable"
  case "$tag" in
    *-*) ;;
    *) put "latest/$name" "$file" "$type" "public, max-age=300" ;;
  esac
done
