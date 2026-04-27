#!/bin/bash

set -euo pipefail

BASE_DIR="$(pwd -P)"

usage() {
    cat <<'EOF'
Usage: create-ota.sh [-b BUILD_DIR] [-o OUTPUT_DIR] [-s SIGN_KEY] [-k]

Create a Rns Mavlink OTA package.

Options:
  -b, --build-dir   Path to build directory
  -o, --output-dir  Path to output directory
  -s, --sign-key    Path to PEM private key used for signing
  -k, --keep        Keep the unpacked OTA directory
  -h, --help        Show this help message
EOF
}

build_dir="${BASE_DIR}/target/armv7-unknown-linux-gnueabihf/release"
output_dir="${BASE_DIR}/deploy"
sign_key=""
keep=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        -b|--build-dir)
            build_dir="$2"
            shift 2
            ;;
        -o|--output-dir)
            output_dir="$2"
            shift 2
            ;;
        -s|--sign-key)
            sign_key="$2"
            shift 2
            ;;
        -k|--keep)
            keep=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

command -v openssl >/dev/null 2>&1 || {
    echo "openssl is required" >&2
    exit 1
}

command -v zip >/dev/null 2>&1 || {
    echo "zip is required" >&2
    exit 1
}

if [[ ! -d "$build_dir" ]]; then
    echo "Build directory not found: $build_dir" >&2
    exit 1
fi

mkdir -p "$output_dir"

build_dir="$(cd "$build_dir" && pwd -P)"
output_dir="$(cd "$output_dir" && pwd -P)"

fc_binary_path="${build_dir}/fc"
gc_binary_path="${build_dir}/gc"
fc_service_path="${BASE_DIR}/rns-mavlink-fc.service"
gc_service_path="${BASE_DIR}/rns-mavlink-gc.service"
fc_config_path="${BASE_DIR}/Fc.toml"
gc_config_path="${BASE_DIR}/Gc.toml"
fc_plugin_toml_path="${BASE_DIR}/kaonic-plugin-fc.toml"
gc_plugin_toml_path="${BASE_DIR}/kaonic-plugin-gc.toml"
release_name="rns-mavlink"
fc_release_path="${output_dir}/${release_name}-fc"
gc_release_path="${output_dir}/${release_name}-gc"
fc_release_archive_path="${output_dir}/${release_name}-fc.zip"
gc_release_archive_path="${output_dir}/${release_name}-gc.zip"

if [[ ! -f "$fc_binary_path" ]]; then
    echo "Fc Binary not found: $fc_binary_path" >&2
    exit 1
fi
if [[ ! -f "$gc_binary_path" ]]; then
    echo "Gc Binary not found: $gc_binary_path" >&2
    exit 1
fi

if [[ ! -f "$fc_service_path" ]]; then
    echo "Fc Service file not found: $fc_service_path" >&2
    exit 1
fi
if [[ ! -f "$gc_service_path" ]]; then
    echo "Gc Service file not found: $gc_service_path" >&2
    exit 1
fi

if [[ ! -f "$fc_config_path" ]]; then
    echo "Fc Config file not found: $fc_config_path" >&2
    exit 1
fi
if [[ ! -f "$gc_config_path" ]]; then
    echo "Gc Config file not found: $gc_config_path" >&2
    exit 1
fi

if [[ ! -f "$fc_plugin_toml_path" ]]; then
    echo "Fc Plugin manifest not found: $fc_plugin_toml_path" >&2
    exit 1
fi
if [[ ! -f "$gc_plugin_toml_path" ]]; then
    echo "Gc Plugin manifest not found: $gc_plugin_toml_path" >&2
    exit 1
fi

if [[ -n "$sign_key" && ! -f "$sign_key" ]]; then
    echo "Signing key not found: $sign_key" >&2
    exit 1
fi

version="$(git describe --tags --long 2>/dev/null || true)"
if [[ -z "$version" ]]; then
    version="v0.0.0"
fi

tmp_dir=""
cleanup() {
    if [[ -n "$tmp_dir" && -d "$tmp_dir" ]]; then
        rm -rf "$tmp_dir"
    fi
}
trap cleanup EXIT

echo "Version: $version"
echo "> Prepare deploy directory"

rm -f "$fc_release_archive_path"
rm -f "$gc_release_archive_path"
rm -rf "$fc_release_path"
rm -rf "$gc_release_path"

echo "> Make directories"
mkdir -p "$fc_release_path/files"
mkdir -p "$gc_release_path/files"

echo "> Copy files"
cp "$fc_binary_path" "$fc_release_path/rns-mavlink-fc"
cp "$gc_binary_path" "$gc_release_path/rns-mavlink-gc"
cp "$fc_service_path" "$fc_release_path/rns-mavlink-fc.service"
cp "$gc_service_path" "$gc_release_path/rns-mavlink-gc.service"
cp "$fc_config_path" "$fc_release_path/files/Fc.toml"
cp "$gc_config_path" "$gc_release_path/files/Gc.toml"
cp "$fc_plugin_toml_path" "$fc_release_path/kaonic-plugin.toml"
cp "$gc_plugin_toml_path" "$gc_release_path/kaonic-plugin.toml"
printf '%s' "$version" > "$fc_release_path/rns-mavlink-fc.version"
printf '%s' "$version" > "$gc_release_path/rns-mavlink-gc.version"
openssl dgst -sha256 "$fc_release_path/rns-mavlink-fc" | awk '{print $NF}' > "$fc_release_path/rns-mavlink-fc.sha256"
openssl dgst -sha256 "$gc_release_path/rns-mavlink-gc" | awk '{print $NF}' > "$gc_release_path/rns-mavlink-gc.sha256"

if [[ -n "$sign_key" ]]; then
    tmp_dir="$(mktemp -d)"
    pub_key_path="${tmp_dir}/ota_sign_key.pub.pem"

    echo "> Sign $fc_release_path/rns-mavlink-fc"
    openssl dgst -sha256 -sign "$sign_key" -out "$fc_release_path/rns-mavlink-fc.sig" "$fc_release_path/rns-mavlink-fc"
    echo "> Sign $gc_release_path/rns-mavlink-gc"
    openssl dgst -sha256 -sign "$sign_key" -out "$gc_release_path/rns-mavlink-gc.sig" "$gc_release_path/rns-mavlink-gc"

    echo "> Verify $fc_release_path/rns-mavlink-fc"
    openssl pkey -in "$sign_key" -pubout -out "$pub_key_path" >/dev/null 2>&1
    openssl dgst -sha256 -verify "$pub_key_path" -signature "$fc_release_path/rns-mavlink-fc.sig" "$fc_release_path/rns-malvink-fc" >/dev/null
    echo "> Verify $gc_release_path/rns-mavlink-gc"
    openssl pkey -in "$sign_key" -pubout -out "$pub_key_path" >/dev/null 2>&1
    openssl dgst -sha256 -verify "$pub_key_path" -signature "$gc_release_path/rns-mavlink-gc.sig" "$gc_release_path/rns-malvink-gc" >/dev/null
fi

(cd "$fc_release_path" && zip -q -r "$fc_release_archive_path" .)
(cd "$gc_release_path" && zip -q -r "$gc_release_archive_path" .)

if [[ "$keep" -ne 1 ]]; then
    rm -rf "$fc_release_path"
    rm -rf "$gc_release_path"
fi

echo "OTA Package: $fc_release_archive_path"
echo "OTA Package: $gc_release_archive_path"
