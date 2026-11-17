#!/usr/bin/env bash
# Build the optimized Windows binary and package it for a GitHub release.
#
#   scripts/build-release.sh              # test, build, package into dist/
#   scripts/build-release.sh --skip-tests # build and package only
#
# Output (version from Cargo.toml):
#   dist/gravingine-image-optimizer-v<version>-windows-x64.zip
#   dist/gravingine-image-optimizer-v<version>-windows-x64.zip.sha256
#
# Runs in Git Bash on Windows (locally and on GitHub's windows runners).
set -euo pipefail

cd "$(dirname "$0")/.."

skip_tests=false
for arg in "$@"; do
    case "$arg" in
        --skip-tests) skip_tests=true ;;
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

echo "==> Packaging ${name}.zip"
rm -rf dist
mkdir -p "dist/${name}"
cp target/release/gravingine_image_optimizer.exe "dist/${name}/gravingine-image-optimizer.exe"
cp README.md "dist/${name}/README.md"
# Git Bash has no zip; Windows PowerShell's Compress-Archive is always there.
powershell.exe -NoProfile -NonInteractive -Command \
    "Compress-Archive -Path 'dist/${name}/*' -DestinationPath 'dist/${name}.zip' -Force"
rm -rf "dist/${name}"

(cd dist && sha256sum "${name}.zip" > "${name}.zip.sha256")

echo "==> Done"
ls -l dist
