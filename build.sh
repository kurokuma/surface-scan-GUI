#!/bin/sh
# Build from any working directory. No toolchains or OS packages are installed.
set -eu

usage() {
    cat <<'HELP'
Usage: sh build.sh [--target TRIPLE] [--development] [--check] [--offline]

  --target TRIPLE  Rust target triple (default: rustc host)
  --development    Use the debug profile instead of release
  --check          Run cargo check; do not produce an executable
  --offline        Do not access the network
  -h, --help       Show this help

All builds use Cargo.lock (--locked) and an explicit --target.
Install target std libraries, a linker and native dependencies separately.
HELP
}

build_target=''
build_configuration=release
build_action=build
build_offline=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            if [ "$#" -lt 2 ] || [ -z "$2" ]; then
                echo 'Error: --target requires a target triple.' >&2
                exit 2
            fi
            build_target=$2
            shift 2
            ;;
        --development) build_configuration=debug; shift ;;
        --check) build_action=check; shift ;;
        --offline) build_offline=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Error: unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

command -v cargo >/dev/null 2>&1 || { echo 'Error: cargo is not on PATH.' >&2; exit 1; }
command -v rustc >/dev/null 2>&1 || { echo 'Error: rustc is not on PATH.' >&2; exit 1; }

if [ -z "$build_target" ]; then
    rust_version=$(rustc -vV)
    build_target=$(printf '%s\n' "$rust_version" | sed -n 's/^host: //p' | tr -d '\r')
fi
case "$build_target" in
    ''|*[!a-zA-Z0-9_.-]*) echo 'Error: invalid target triple.' >&2; exit 2 ;;
esac

build_root=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
cd "$build_root"
[ -f Cargo.lock ] || { echo 'Error: Cargo.lock is missing.' >&2; exit 1; }
set -- "$build_action" --locked --target "$build_target"
if [ "$build_configuration" = release ]; then set -- "$@" --release; fi
if [ "$build_offline" -eq 1 ]; then set -- "$@" --offline; fi

printf 'Running: cargo'
printf ' %s' "$@"
printf '\n'
cargo "$@"
if [ "$build_action" = check ]; then
    printf 'Check completed: %s (%s); no executable produced.\n' "$build_target" "$build_configuration"
else
    printf 'Build completed: %s (%s). See README for output paths.\n' "$build_target" "$build_configuration"
fi
