#!/usr/bin/env bash
# Build the optimized Windows binary and package it for a GitHub release.
#
#   scripts/build-release.sh                       # test, build, zip and installer into dist/
#   scripts/build-release.sh --skip-tests          # build and package only
#   scripts/build-release.sh --require-installer   # fail (instead of skipping the installer)
#                                                  # when Inno Setup isn't installed
#
# Output (version from Cargo.toml):
#   dist/gravingine-image-optimizer-v<version>-windows-x64.zip          portable: exe, README, LICENSE
#   dist/gravingine-image-optimizer-v<version>-windows-x64-setup.exe    installer (Inno Setup 6)
#   a .sha256 checksum next to each
#
# The installer needs Inno Setup 6 (ISCC.exe): on PATH, in $ISCC, or in its default per-user
# or all-users install folder (winget install JRSoftware.InnoSetup).
#
# Runs in Git Bash on Windows (locally and on GitHub's windows runners).
set -euo pipefail

cd "$(dirname "$0")/.."

skip_tests=false
require_installer=false
for arg in "$@"; do
    case "$arg" in
        --skip-tests) skip_tests=true ;;
        --require-installer) require_installer=true ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
    echo "couldn't read the version from Cargo.toml" >&2
    exit 1
fi
name="gravingine-image-optimizer-v${version}-windows-x64"
echo "==> Gravingine Image Optimizer v${version}"

if [ "$skip_tests" = false ]; then
    echo "==> Running tests"
    cargo test --locked
fi

echo "==> Building release binary"
cargo build --release --locked

echo "==> Staging files"
rm -rf dist
stage="dist/${name}"
mkdir -p "$stage"
cp target/release/gravingine_image_optimizer.exe "$stage/gravingine-image-optimizer.exe"
cp README.md "$stage/README.md"
cp LICENSE "$stage/LICENSE.txt"

echo "==> Packaging ${name}.zip"
# Git Bash has no zip; Windows PowerShell's Compress-Archive is always there.
powershell.exe -NoProfile -NonInteractive -Command \
    "Compress-Archive -Path '${stage}/*' -DestinationPath 'dist/${name}.zip' -Force"

# Inno Setup's compiler: $ISCC, PATH, or its default install folders.
iscc="${ISCC:-}"
if [ -z "$iscc" ]; then
    for candidate in \
        "$(command -v iscc || true)" \
        "$(command -v ISCC || true)" \
        "${LOCALAPPDATA:-}/Programs/Inno Setup 6/ISCC.exe" \
        "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" \
        "/c/Program Files/Inno Setup 6/ISCC.exe"; do
        if [ -n "$candidate" ] && [ -f "$candidate" ]; then
            iscc="$candidate"
            break
        fi
    done
fi

if [ -n "$iscc" ]; then
    echo "==> Building installer ${name}-setup.exe"
    # The .ico build.rs generated for the exe: the newest one under the release build folder.
    # (Searched by name: Cargo's folder layout differs between versions, e.g.
    # build/gravingine_image_optimizer-<hash>/out and build/gravingine_image_optimizer/<hash>/out.)
    icon=$(find target/release/build -path '*gravingine_image_optimizer*' -name app.ico -printf '%T@ %p\n' \
        | sort -n | tail -n 1 | cut -d' ' -f2-)
    if [ -z "$icon" ]; then
        echo "build.rs didn't generate app.ico under target/release/build" >&2
        exit 1
    fi
    # Absolute Windows paths: ISCC resolves relative ones against the script's folder.
    win() { cygpath -wa "$1"; }
    # Keep Git Bash from rewriting the /D... options as paths.
    MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL="*" "$iscc" -Q \
        "/DAppVersion=${version}" \
        "/DSourceDir=$(win "$stage")" \
        "/DSetupIcon=$(win "$icon")" \
        "/DOutputDir=$(win dist)" \
        "$(win installer/gravingine.iss)"
elif [ "$require_installer" = true ]; then
    echo "Inno Setup 6 (ISCC.exe) not found; install it or set ISCC" >&2
    exit 1
else
    echo "==> Skipping the installer: Inno Setup 6 (ISCC.exe) not found"
fi
rm -rf "$stage"

echo "==> Checksums"
(cd dist && for f in *.zip *.exe; do [ -f "$f" ] && sha256sum "$f" > "$f.sha256"; done; true)

echo "==> Done"
ls -l dist
