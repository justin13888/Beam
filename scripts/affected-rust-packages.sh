#!/bin/sh
# affected-rust-packages.sh FILE... -- the cargo package selection a change
# needs, for `rust:clippy:affected` and `rust:test:affected` (#478).
#
# Prints the arguments to splice into a cargo command, on one line:
#
#   * `-p NAME` for every workspace member that owns one of FILE, plus every
#     member that depends on one of those (normal, build or dev edges), so a
#     change is linted and tested wherever it is used;
#   * `--workspace` instead, when a FILE is a workspace-wide input: the root
#     Cargo.toml, Cargo.lock, rust-toolchain.toml, clippy.toml or .cargo/**;
#   * `--features LIST` with the BEAM_CARGO_FEATURES entries that apply to the
#     selection. cargo rejects `pkg/feature` for a package that is not
#     selected, so a qualified entry is kept only when its package is.
#
# Prints nothing when no FILE belongs to a workspace member; the caller then
# skips cargo entirely. FILE paths are relative to the repository root, which
# is where mise runs tasks and hk runs steps.
#
# Members and their dependents come from `cargo tree`, which reads the
# manifests and the lockfile and compiles nothing, so this costs well under a
# second and never drifts from the workspace's own member list.
set -eu

root=$(pwd)
workspace=false

# NAME REL-DIR per member, e.g. `beam-domain beam-domain`.
members=$(cargo tree --locked --color never --workspace --depth 0 --prefix none --format '{p}' |
    sed -n 's/^\([^ ]*\) v[^ ]* (\(.*\))$/\1 \2/p' |
    sort -u |
    while read -r name dir; do
        printf '%s %s\n' "$name" "${dir#"$root"/}"
    done)

owned=""
for file in "$@"; do
    file=${file#./}
    case "$file" in
        Cargo.toml | Cargo.lock | rust-toolchain.toml | clippy.toml | .clippy.toml | .cargo/*)
            workspace=true
            continue
            ;;
    esac
    # The longest member directory that contains the file, so a member nested
    # in another (none is today) is not mistaken for its parent.
    best=""
    best_len=0
    while read -r name dir; do
        case "$file" in
            "$dir"/*)
                if [ "${#dir}" -gt "$best_len" ]; then
                    best=$name
                    best_len=${#dir}
                fi
                ;;
        esac
    done <<EOF
$members
EOF
    if [ -n "$best" ]; then
        owned="$owned $best"
    fi
done

features=${BEAM_CARGO_FEATURES:-}

if [ "$workspace" = true ]; then
    printf '%s' "--workspace"
    if [ -n "$features" ]; then
        printf ' --features %s' "$features"
    fi
    printf '\n'
    exit 0
fi

if [ -z "$owned" ]; then
    exit 0
fi

member_names=$(printf '%s\n' "$members" | cut -d' ' -f1)

# Each owner and every package that depends on it, filtered to members below.
selected=$(for name in $(printf '%s\n' $owned | sort -u); do
    cargo tree --locked --color never --workspace -i "$name" -e normal,build,dev \
        --prefix none --format '{p}' | cut -d' ' -f1
done | sort -u)
args=""
for name in $selected; do
    if printf '%s\n' "$member_names" | grep -qFx "$name"; then
        args="$args -p $name"
    fi
done

kept=""
old_ifs=$IFS
IFS=,
for feature in $features; do
    case "$feature" in
        */*)
            case " $args " in
                *" -p ${feature%%/*} "*) ;;
                *) continue ;;
            esac
            ;;
    esac
    kept="${kept:+$kept,}$feature"
done
IFS=$old_ifs

printf '%s' "${args# }"
if [ -n "$kept" ]; then
    printf ' --features %s' "$kept"
fi
printf '\n'
