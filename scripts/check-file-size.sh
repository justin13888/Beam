#!/bin/sh
# check:file-size -- no Rust or Kotlin file grows without a bound (#362).
#
# Every tracked file whose extension has a `limit` line in the baseline fails
# the check when its total line count (what an editor's gutter shows, a final
# line without a newline included) exceeds that limit -- unless its path is on
# the baseline list. The baseline is shrink-only, and the check enforces both
# directions of that itself:
#
#   * an entry whose file is gone, or is back within its limit, fails the
#     check until it is deleted, so every split shrinks the list for good;
#   * an entry absent from the base revision's baseline fails the check, so a
#     path cannot be added after the gate landed. The one exception is a
#     change to the `limit` lines themselves, which is the only time the list
#     may be regenerated and grow.
#
# The base revision is $BEAM_FILE_SIZE_BASE when set (CI sets it to the pull
# request's base, or the previous master on a push), else the merge base with
# origin/master. With neither available the shrink-only comparison is skipped
# and says so; the size check itself always runs.
#
# Only tracked files are read (`git ls-files`), so build output under target/
# and build/ is never counted, and the generated OpenAPI document is JSON.
set -eu
# No globbing: the `*.EXT` pathspecs below are for git, not the shell.
set -f

baseline=scripts/file-size-baseline.txt
status=0

fail() {
    echo "check:file-size: $*" >&2
    status=1
}

# Limits are `limit .EXT N` lines; every other non-comment line is a path.
limits=$(awk '$1 == "limit" { print $2, $3 }' "$baseline")
paths=$(awk '!/^[ \t]*(#|$)/ && $1 != "limit" { print $1 }' "$baseline")

if [ -z "$limits" ]; then
    fail "$baseline declares no limit lines"
    exit 1
fi

# A sorted, duplicate-free list keeps every diff to it a pure removal.
if ! printf '%s\n' "$paths" | LC_ALL=C sort -c -u 2>/dev/null; then
    fail "$baseline paths must be sorted (LC_ALL=C) and unique"
fi

limit_for() {
    printf '%s\n' "$limits" | awk -v ext=".${1##*.}" '$1 == ext { print $2; exit }'
}

line_count() {
    awk 'END { print NR }' "$1"
}

in_baseline() {
    printf '%s\n' "$paths" | grep -qxF -- "$1"
}

# 1. Every governed file is within its limit, or on the baseline.
patterns=$(printf '%s\n' "$limits" | awk '{ printf "*%s\n", $1 }')
# shellcheck disable=SC2086 # the patterns are word-split on purpose
for file in $(git ls-files -- $patterns); do
    # `git ls-files` still lists a file deleted in the working tree.
    [ -f "$file" ] || continue
    limit=$(limit_for "$file")
    count=$(line_count "$file")
    if [ "$count" -gt "$limit" ] && ! in_baseline "$file"; then
        fail "$file has $count lines, over the limit of $limit -- split it; the baseline may not grow"
    fi
done

# 2. Every baseline entry still needs to be there.
for file in $paths; do
    limit=$(limit_for "$file")
    if [ -z "$limit" ]; then
        fail "$baseline lists $file, whose extension has no limit"
    elif ! git ls-files --error-unmatch -- "$file" >/dev/null 2>&1 || [ ! -f "$file" ]; then
        fail "$baseline lists $file, which is not a tracked file -- delete the entry"
    elif [ "$(line_count "$file")" -le "$limit" ]; then
        fail "$file is within its limit of $limit now -- delete its entry from $baseline"
    fi
done

# 3. Nothing was added to the baseline relative to the base revision.
base=${BEAM_FILE_SIZE_BASE:-}
if [ -z "$base" ]; then
    base=$(git merge-base HEAD origin/master 2>/dev/null || true)
fi
if [ -z "$base" ]; then
    echo "check:file-size: origin/master not available and BEAM_FILE_SIZE_BASE unset; skipping the shrink-only comparison"
elif ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
    fail "base revision $base is not in this clone -- fetch it, or unset BEAM_FILE_SIZE_BASE"
elif base_baseline=$(git show "$base:$baseline" 2>/dev/null); then
    base_limits=$(printf '%s\n' "$base_baseline" | awk '$1 == "limit" { print $2, $3 }')
    if [ "$base_limits" = "$limits" ]; then
        base_paths=$(printf '%s\n' "$base_baseline" | awk '!/^[ \t]*(#|$)/ && $1 != "limit" { print $1 }')
        for file in $paths; do
            if ! printf '%s\n' "$base_paths" | grep -qxF -- "$file"; then
                fail "$file was added to $baseline -- the baseline only shrinks; split the file instead"
            fi
        done
    else
        echo "check:file-size: limits changed since $base; the baseline may be regenerated in this change"
    fi
else
    echo "check:file-size: $base has no $baseline; this change introduces it"
fi

if [ "$status" -eq 0 ]; then
    echo "every Rust and Kotlin file is within its limit or on the shrink-only baseline"
fi
exit "$status"
