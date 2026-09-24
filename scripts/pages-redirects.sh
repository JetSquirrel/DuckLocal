#!/bin/bash
# The old documentation address, jetsquirrel.github.io/DuckLocal/, as redirects.
#
# The docs moved to ducklocal.app/docs/ and the product page to ducklocal.app;
# links to the old site live on in READMEs, issues and search results. GitHub
# Pages cannot answer with a real redirect, so every page it used to serve is
# a small HTML file that points at the new address — a canonical link and a
# meta refresh for crawlers, a script that keeps the #anchor for readers — and
# 404.html does the same for any path not listed.
#
# Usage: scripts/pages-redirects.sh OUT_DIR
set -euo pipefail

out="${1:?usage: $0 OUT_DIR}"
docs="https://ducklocal.app/docs/"
home="https://ducklocal.app/"

# Every page the old site published, under both its English and Chinese
# addresses; `index` is the site's home.
pages=(
    index getting-started tutorial troubleshooting data-sources s3 sql-editor
    schema-and-history results-and-charts settings-and-data cli analysis-app
    dashboards development
)

rm -rf "$out"
mkdir -p "$out/zh"

# page FILE TARGET LANG: one redirect page.
page() {
    local file="$1" target="$2" lang="$3"
    cat > "$out/$file" <<EOF
<!doctype html>
<html lang="$lang">
<head>
<meta charset="utf-8">
<meta name="robots" content="noindex">
<title>DuckLocal has moved</title>
<link rel="canonical" href="$target">
<meta http-equiv="refresh" content="0; url=$target">
<script>location.replace("$target" + location.hash)</script>
</head>
<body><p>This page has moved to <a href="$target">$target</a>.</p></body>
</html>
EOF
}

for name in "${pages[@]}"; do
    if [ "$name" = index ]; then
        page index.html "$home" en
        page zh/index.html "${home}zh/" zh-CN
        page index.zh-CN.html "${home}zh/" zh-CN
    else
        page "$name.html" "$docs$name" en
        page "zh/$name.html" "${docs}zh/$name" zh-CN
        # The addresses the first Chinese pages had, before they moved to zh/.
        page "$name.zh-CN.html" "${docs}zh/$name" zh-CN
    fi
done

# Anything else under the old base: the same path on the docs site, without
# the base and the .html the new site no longer needs.
cat > "$out/404.html" <<EOF
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="robots" content="noindex">
<title>DuckLocal has moved</title>
<script>
  var path = location.pathname.replace(/^\/DuckLocal\//, "").replace(/\.html$/, "");
  location.replace("$docs" + path + location.hash);
</script>
</head>
<body><p>The DuckLocal docs have moved to <a href="$docs">$docs</a>.</p></body>
</html>
EOF

echo "Wrote $(find "$out" -name '*.html' | wc -l | tr -d ' ') redirect pages to $out"
