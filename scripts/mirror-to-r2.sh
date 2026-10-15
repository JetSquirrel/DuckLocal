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
# Then the release's metadata, last, so it never names a package that is not
# there yet: <tag>/SHA256SUMS, <tag>/manifest.json (each package's platform,
# URL, size and SHA-256), and — for a stable release — the same manifest as
# latest.json, the feed DuckLocal's updater reads (src/updater/manifest.rs).
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

sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# The platform a package is for, as Rust's std::env::consts say it, and its
# format: "os arch format".
platform() {
  case "$1" in
    ducklocal-macos-arm64.dmg) echo "macos aarch64 dmg" ;;
    ducklocal-linux-x86_64.tar.gz) echo "linux x86_64 tar.gz" ;;
    ducklocal-windows-x86_64.zip) echo "windows x86_64 zip" ;;
    *) return 1 ;;
  esac
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

meta=$(mktemp -d)
sums="$meta/SHA256SUMS"
assets=""
for file in "$@"; do
  name=$(basename "$file")
  set -- $(platform "$name") || { echo "no platform for $name" >&2; exit 1; }
  os=$1 arch=$2 format=$3
  digest=$(sha256 "$file")
  size=$(wc -c < "$file" | tr -d ' ')
  min_os=""
  [ "$os" = macos ] && min_os='"min_os": "12.0", '
  printf '%s  %s\n' "$digest" "$name" >> "$sums"
  assets="$assets${assets:+,
}    {\"os\": \"$os\", \"arch\": \"$arch\", \"format\": \"$format\", $min_os\"url\": \"https://dl.ducklocal.app/$tag/$name\", \"size\": $size, \"sha256\": \"$digest\"}"
done
cat > "$meta/manifest.json" <<JSON
{
  "version": "${tag#v}",
  "notes_url": "https://github.com/JetSquirrel/DuckLocal/releases/tag/$tag",
  "assets": [
$assets
  ]
}
JSON
put "$tag/SHA256SUMS" "$sums" text/plain "public, max-age=31536000, immutable"
put "$tag/manifest.json" "$meta/manifest.json" application/json \
  "public, max-age=31536000, immutable"
case "$tag" in
  *-*) ;;
  *) put "latest.json" "$meta/manifest.json" application/json "public, max-age=300" ;;
esac
rm -rf "$meta"
