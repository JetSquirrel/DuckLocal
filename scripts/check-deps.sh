#!/bin/bash
# The dependency invariants a build can break silently.
#
# The toolkit's crates come out of one repository, and they only work together
# when all of them are the same release: two copies put two `gpui-base` crates
# in the build, and the failure is a wall of trait mismatches that names
# neither cause. Nothing in Cargo enforces that, so this does, offline and in
# a second. The toolkit may be pinned either way, but only one way at a time:
# a single crates.io version, or a single Git revision of its repository.
#
# Run from anywhere; CI runs it before the tests.
set -euo pipefail

cd "$(dirname "$0")/.."

REPO="github.com/longbridge/gpui-kit"
# The crates one toolkit release is made of.
TOOLKIT='^(gpui-kit|gpui-kit-assets|gpui-base|gpui-component|gpui-component-macros)$'
status=0

fail() {
    echo "  ✗ $*" >&2
    status=1
}

give_up() {
    echo "Dependency check failed." >&2
    exit 1
}

# 1. One pin in Cargo.toml, across every dependency on the toolkit.
manifest_revs=$(
    grep -o "git = \"https://$REPO\", rev = \"[0-9a-f]*\"" Cargo.toml |
        grep -o 'rev = "[0-9a-f]*"' | grep -o '[0-9a-f]\{7,\}' | sort -u || true
)
manifest_versions=$(
    grep -E '^gpui-kit[[:space:]]*=' Cargo.toml | grep -v 'git = ' |
        sed -nE 's/^gpui-kit[[:space:]]*=[[:space:]]*(\{.*version[[:space:]]*=[[:space:]]*)?"([^"]+)".*/\2/p' |
        sort -u || true
)
rev_count=$(printf '%s\n' "$manifest_revs" | grep -c . || true)
version_count=$(printf '%s\n' "$manifest_versions" | grep -c . || true)

if [ "$rev_count" -gt 0 ] && [ "$version_count" -gt 0 ]; then
    fail "Cargo.toml takes gpui-kit both from $REPO and from crates.io; pick one."
    give_up
fi
if [ "$rev_count" -eq 0 ] && [ "$version_count" -eq 0 ]; then
    fail "Cargo.toml pins no gpui-kit: neither a version nor a rev = \"...\" of $REPO."
    give_up
fi
# Everything below compares against "the" pin, so there is nothing sensible to
# say until there is exactly one.
if [ "$rev_count" -gt 1 ]; then
    fail "Cargo.toml pins $REPO at more than one revision:"
    printf '      %s\n' $manifest_revs >&2
    give_up
fi
if [ "$version_count" -gt 1 ]; then
    fail "Cargo.toml asks for more than one version of gpui-kit:"
    printf '      %s\n' $manifest_versions >&2
    give_up
fi

git_sources=$(grep -o "git+https://$REPO?rev=[0-9a-f]*#[0-9a-f]*" Cargo.lock | sort -u || true)

if [ "$rev_count" -eq 1 ]; then
    rev=$manifest_revs
    pinned="$REPO pinned at $rev"

    # 2. Cargo.lock agrees, and every rev resolved to the commit it asked for.
    while IFS= read -r source; do
        [ -n "$source" ] || continue
        asked=${source#*rev=}
        asked=${asked%%#*}
        resolved=${source#*#}
        [ "$asked" = "$rev" ] || fail "Cargo.lock asks $REPO for $asked, Cargo.toml pins $rev."
        # A branch or a tag can be pinned too, and then what was asked for and
        # what the lock holds are different strings that both belong here.
        [ "$resolved" = "$asked" ] ||
            fail "Cargo.lock asked $REPO for $asked and got $resolved; the pin moved."
    done <<< "$git_sources"
else
    pinned="gpui-kit $manifest_versions from crates.io"

    # 2. Cargo.lock holds nothing from the repository: a stale lock, or a
    #    [patch] that brings the Git copy back in beside the release.
    if [ -n "$git_sources" ]; then
        fail "Cargo.toml takes gpui-kit from crates.io, but Cargo.lock still has crates from $REPO:"
        printf '      %s\n' $git_sources >&2
    fi
fi

# 3. No crate is in the build twice, once from the repository and once from
#    crates.io: the shape the same mistake takes for any shared crate.
duplicates=$(
    awk -v repo="$REPO" '
        /^name = / { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name) }
        /^source = / {
            source = $0; sub(/^source = "/, "", source); sub(/"$/, "", source)
            if (index(source, repo)) { git[name] = 1 } else { elsewhere[name] = 1 }
        }
        END { for (n in git) if (n in elsewhere) print n }
    ' Cargo.lock
)
if [ -n "$duplicates" ]; then
    fail "these crates are in the build twice, from $REPO and from elsewhere:"
    printf '      %s\n' $duplicates >&2
fi

# 4. No toolkit crate is in the build at two versions, whatever the source.
versions_twice=$(
    awk -v toolkit="$TOOLKIT" '
        /^name = / { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name) }
        /^version = / { if (name ~ toolkit) count[name]++ }
        END { for (n in count) if (count[n] > 1) print n }
    ' Cargo.lock
)
if [ -n "$versions_twice" ]; then
    fail "these toolkit crates are in the build at more than one version:"
    printf '      %s\n' $versions_twice >&2
fi

if [ "$status" -ne 0 ]; then
    give_up
fi

echo "Dependencies check out: $pinned, one copy of each crate."
