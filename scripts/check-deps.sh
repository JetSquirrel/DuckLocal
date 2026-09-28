#!/bin/bash
# The dependency invariants a build can break silently.
#
# The toolkit, the script runtime and the component catalog are three crates
# out of one repository, and `rquickjs` is patched to the same one. They only
# work together when all four come from the same commit: two revisions put two
# copies of `gpui-base` — or of `rquickjs` — in the build, and the failure is a
# wall of trait mismatches that names neither cause. Nothing in Cargo enforces
# that, so this does, offline and in a second.
#
# Run from anywhere; CI runs it before the tests.
set -euo pipefail

cd "$(dirname "$0")/.."

REPO="github.com/longbridge/gpui-kit"
status=0

fail() {
    echo "  ✗ $*" >&2
    status=1
}

# 1. One pin in Cargo.toml, across dependencies and [patch] alike. A pin is a
#    rev, a tag or a branch; all four entries must use the same one.
manifest_pins=$(
    grep -oE "git = \"https://$REPO\", (rev|tag|branch) = \"[^\"]+\"" Cargo.toml |
        sed -E 's/.*(rev|tag|branch) = "([^"]+)"/\1 = \2/' | sort -u
)
count=$(printf '%s\n' "$manifest_pins" | grep -c . || true)
if [ "$count" -eq 0 ]; then
    fail "Cargo.toml pins nothing from $REPO; a rev/tag/branch = \"...\" is how it is pinned."
    echo "Dependency check failed." >&2
    exit 1
fi
if [ "$count" -ne 1 ]; then
    # Everything below compares against "the" pin, so there is nothing
    # sensible to say until there is exactly one.
    fail "Cargo.toml pins $REPO at more than one revision:"
    printf '      %s\n' $manifest_pins >&2
    echo "Dependency check failed." >&2
    exit 1
fi
pin=$manifest_pins
kind=${pin%% = *}
value=${pin#* = }

# 2. Cargo.lock agrees: every crate from the repository was asked for the
#    manifest's pin. A rev resolves to itself; a tag or branch resolves to the
#    commit it names, which the same query string already pins down.
seen=0
while IFS= read -r source; do
    seen=1
    query=${source#*\?}
    query=${query%%#*}
    resolved=${source##*#}
    [ "$query" = "$kind=$value" ] ||
        fail "Cargo.lock asks $REPO for $query, Cargo.toml pins $kind = $value."
    if [ "$kind" = rev ] && [ "$resolved" != "$value" ]; then
        fail "Cargo.lock asked $REPO for $value and got $resolved; the pin moved."
    fi
done < <(grep -oE "git\+https://$REPO\?(rev|tag|branch)=[^#\"]+#[0-9a-f]+" Cargo.lock | sort -u)
[ "$seen" -eq 1 ] || fail "Cargo.lock holds nothing from $REPO."

# 3. No crate is in the build twice, once from the repository and once from
#    crates.io. This is what the [patch] on rquickjs exists to prevent, and it
#    is the shape the same mistake takes for any other shared crate.
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

if [ "$status" -ne 0 ]; then
    echo "Dependency check failed." >&2
    exit 1
fi

echo "Dependencies check out: $REPO pinned at $pin, one copy of each crate."
