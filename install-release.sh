#!/bin/sh
# Standalone Mac installer for the pinned official release. No language runtime is required.
set -eu
version=0.4.0
assets_dir=
prefix="$HOME/.local"
setup=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --assets-dir|--prefix)
            [ "$#" -ge 2 ] || { echo "Missing value for $1" >&2; exit 2; }
            case "$1" in --assets-dir) assets_dir=$2 ;; --prefix) prefix=$2 ;; esac
            shift 2 ;;
        --setup) setup=true; shift ;;
        --help|-h)
            printf '%s\n' 'Usage: install-release.sh [--assets-dir DIRECTORY] [--prefix DIRECTORY] [--setup]' \
                'Installs ClipBridge 0.4.0 for this Mac. --setup opens the local/remote configuration wizard.'
            exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done
[ "$(uname -s)" = Darwin ] || { echo 'Run this installer on the Mac whose clipboard you want to synchronize.' >&2; exit 1; }
[ "$(id -u)" != 0 ] || { echo 'Run this installer as your normal user, without sudo.' >&2; exit 1; }
case "$(uname -m)" in
    arm64|aarch64) target=aarch64-apple-darwin ;;
    x86_64) target=x86_64-apple-darwin ;;
    *) echo 'This Mac architecture has no precompiled release.' >&2; exit 1 ;;
esac
case "$prefix" in /*) ;; *) echo '--prefix must be an absolute path.' >&2; exit 2 ;; esac
installation="$prefix/share/clipbridge"
if [ -f "$installation/.clipbridge-install.json" ]; then
    [ ! -L "$installation/.clipbridge-install.json" ] || { echo 'Refusing a symlink installation marker.' >&2; exit 1; }
    [ "$(/usr/bin/plutil -extract product raw -o - "$installation/.clipbridge-install.json")" = clipbridge ] \
        || { echo 'Unrecognized existing installation.' >&2; exit 1; }
    current=$(readlink "$installation/current") || { echo 'Existing installation has no managed current release.' >&2; exit 1; }
    case "$current" in releases/*) release=${current#releases/} ;; *) echo 'Existing release link is invalid.' >&2; exit 1 ;; esac
    case "$release" in ''|.|..|*[!a-zA-Z0-9._-]*) echo 'Existing release link is invalid.' >&2; exit 1 ;; esac
    marker="$installation/$current/.clipbridge-release.json"
    [ -f "$marker" ] && [ ! -L "$marker" ] || { echo 'Existing release marker is invalid.' >&2; exit 1; }
    installed=$(/usr/bin/plutil -extract version raw -o - "$marker")
    printf '%s\n' "$installed" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
        || { echo 'Existing release version is invalid.' >&2; exit 1; }
    if awk -v old="$installed" -v new="$version" 'BEGIN {split(old,o,"."); split(new,n,"."); for(i=1;i<=3;i++){if(o[i]+0>n[i]+0)exit 0;if(o[i]+0<n[i]+0)exit 1}exit 1}'; then
        echo "Refusing to downgrade installed ClipBridge $installed to $version. Use a matching or newer installer." >&2
        exit 1
    fi
fi
if [ -n "$assets_dir" ]; then
    assets_dir=$(CDPATH= cd -- "$assets_dir" && pwd -P) \
        || { echo '--assets-dir must name an existing directory.' >&2; exit 1; }
fi
name="clipbridge-$version-$target"
asset="$name.tar.gz"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/clipbridge-install.XXXXXX")
trap 'rm -rf -- "$temporary"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
umask 077
fail() { printf '%s\n' "$*" >&2; exit 1; }
size() { wc -c < "$1" | tr -d ' '; }
hash_file() { shasum -a 256 "$1" | awk '{print $1}'; }
fetch() {
    requested=$1
    limit=$2
    if [ -n "$assets_dir" ]; then
        [ -f "$assets_dir/$requested" ] && [ ! -L "$assets_dir/$requested" ] || fail "Missing regular release asset: $requested"
        [ "$(size "$assets_dir/$requested")" -le "$limit" ] || fail "Release asset exceeds size limit: $requested"
        cp -- "$assets_dir/$requested" "$temporary/$requested"
    else
        /usr/bin/curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
            --tlsv1.2 --connect-timeout 15 --max-time 180 --max-filesize "$limit" \
            --output "$temporary/$requested" \
            "https://github.com/ruoye-ruolan/clip-bridge/releases/download/v$version/$requested" \
            || fail 'Download failed. Retry or use --assets-dir with the matching official release assets.'
    fi
    [ "$(size "$temporary/$requested")" -gt 0 ] && [ "$(size "$temporary/$requested")" -le "$limit" ] || fail 'Invalid release asset size.'
}
fetch "$asset.sha256" 16384
fetch "$asset" 134217728
# A sidecar may name only this exact asset, with one 64-digit digest.
expected=$(awk -v asset="$asset" '
    NR == 1 && length($0) == 66 + length(asset) && substr($0,65,2) == "  " && substr($0,67) == asset {
        hash=substr($0,1,64); if (hash !~ /[^0123456789abcdefABCDEF]/) valid=1
    }
    END { if (NR != 1 || !valid) exit 1; print tolower(hash) }
' "$temporary/$asset.sha256") || fail 'Invalid checksum sidecar.'
[ "$(hash_file "$temporary/$asset")" = "$expected" ] || fail 'Archive SHA-256 mismatch. Nothing was installed.'

# Never extract paths or links from an archive. Validate its complete inventory,
# then stream each regular member into an independently created private directory.
(ulimit -f 8192; tar -tzf "$temporary/$asset") > "$temporary/entries" || fail 'Cannot read release archive.'
(ulimit -f 8192; tar -tvzf "$temporary/$asset") > "$temporary/types" || fail 'Cannot inspect release archive.'
awk 'substr($0,1,1) != "-" || / link to / || / -> / {bad=1} END {exit bad}' "$temporary/types" \
    || fail 'Release archive contains a link, directory, or special file.'
[ "$(LC_ALL=C sort "$temporary/entries" | uniq -d | wc -l | tr -d ' ')" = 0 ] || fail 'Release archive contains duplicate paths.'
while IFS= read -r entry; do
    case "$entry" in "$name/"*) relative=${entry#"$name/"} ;; *) fail 'Archive member escaped the release directory.' ;; esac
    case "$relative" in
        clipbridge|install.sh|bundle.json|Cargo.toml|Cargo.lock|VERSION|LICENSE|README.md|AGENTS.md|src/README.md|src/config.example.json|docs/installation.md|docs/usage.md|docs/remote-images.md|docs/plugins.md|docs/planning/product-direction-and-distribution.md) ;;
        *) fail "Unexpected release archive member: $relative" ;;
    esac
done < "$temporary/entries"
bundle="$temporary/$name"
mkdir "$bundle"
while IFS= read -r entry; do
    relative=${entry#"$name/"}
    mkdir -p -- "$bundle/$(dirname -- "$relative")"
    (ulimit -f 131072; tar -xOzf "$temporary/$asset" "$entry") > "$bundle/$relative" || fail "Cannot unpack $relative"
    [ "$(size "$bundle/$relative")" -le 67108864 ] || fail 'Expanded release file exceeds size limit.'
done < "$temporary/entries"

manifest="$bundle/bundle.json"
field() { /usr/bin/plutil -extract "$1" raw -o - "$manifest"; }
# plutil's extraction mode accepts JSON; its plist-only lint mode does not.
[ "$(field product)" = clipbridge ] && [ "$(field schema)" = 1 ] || fail 'Unrecognized native bundle.'
[ "$(field version)" = "$version" ] && [ "$(field target)" = "$target" ] || fail 'Native bundle version or architecture mismatch.'
[ "$(cat "$bundle/VERSION")" = "$version" ] || fail 'Bundle VERSION mismatch.'
[ "$(hash_file "$bundle/clipbridge")" = "$(field binary_sha256)" ] || fail 'Native executable checksum mismatch.'
for required in Cargo.toml Cargo.lock LICENSE src/config.example.json; do
    [ -s "$bundle/$required" ] || fail "Missing native bundle payload: $required"
done
chmod 755 "$bundle/clipbridge"
"$bundle/clipbridge" install --source "$bundle" --prefix "$prefix"
if [ "$setup" = true ]; then
    # Remove extraction state before exec, but leave stdin connected to the caller's terminal.
    rm -rf -- "$temporary"
    trap - EXIT HUP INT TERM
    if [ -n "$assets_dir" ]; then
        exec "$prefix/bin/clipbridge" setup --assets-dir "$assets_dir"
    fi
    exec "$prefix/bin/clipbridge" setup
fi
