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

# 1. One revision in Cargo.toml, across dependencies and [patch] alike.
manifest_revs=$(
    grep -o "git = \"https://$REPO\", rev = \"[0-9a-f]*\"" Cargo.toml |
        grep -o 'rev = "[0-9a-f]*"' | grep -o '[0-9a-f]\{7,\}' | sort -u
)
count=$(printf '%s\n' "$manifest_revs" | grep -c . || true)
if [ "$count" -eq 0 ]; then
    fail "Cargo.toml pins nothing from $REPO; a rev = \"...\" is how it is pinned."
    echo "Dependency check failed." >&2
    exit 1
fi
if [ "$count" -ne 1 ]; then
    # Everything below compares against "the" revision, so there is nothing
    # sensible to say until there is exactly one.
    fail "Cargo.toml pins $REPO at more than one revision:"
    printf '      %s\n' $manifest_revs >&2
    echo "Dependency check failed." >&2
    exit 1
fi
rev=$manifest_revs

# 2. Cargo.lock agrees, and every rev resolved to the commit it asked for.
while IFS= read -r source; do
    asked=${source#*rev=}
    asked=${asked%%#*}
    resolved=${source#*#}
    [ "$asked" = "$rev" ] || fail "Cargo.lock asks $REPO for $asked, Cargo.toml pins $rev."
    # A branch or a tag can be pinned too, and then what was asked for and
    # what the lock holds are different strings that both belong here.
    [ "$resolved" = "$asked" ] ||
        fail "Cargo.lock asked $REPO for $asked and got $resolved; the pin moved."
done < <(grep -o "git+https://$REPO?rev=[0-9a-f]*#[0-9a-f]*" Cargo.lock | sort -u)

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

echo "Dependencies check out: $REPO pinned at $rev, one copy of each crate."
