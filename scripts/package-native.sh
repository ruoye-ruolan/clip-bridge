#!/bin/bash
# Package an explicitly built binary; this script never builds or installs it.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
target=
binary=
output="$root/dist"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target|--binary|--output-dir)
            [ "$#" -ge 2 ] || { echo "Missing value for $1" >&2; exit 2; }
            case "$1" in
                --target) target=$2 ;;
                --binary) binary=$2 ;;
                --output-dir) output=$2 ;;
            esac
            shift 2 ;;
        *) echo "Usage: $0 --target TARGET --binary PATH [--output-dir DIR]" >&2; exit 2 ;;
    esac
done
[ -n "$target" ] && [ -n "$binary" ] || { echo 'Supply --target and --binary.' >&2; exit 2; }
[ -f "$binary" ] && [ ! -L "$binary" ] || { echo 'Binary must be a regular file, not a symlink.' >&2; exit 1; }
case "$target" in
    aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-musl|aarch64-unknown-linux-musl) ;;
    *) echo "Unsupported target: $target" >&2; exit 2 ;;
esac
version=$(tr -d '\r\n' < "$root/VERSION")
manifest_version=$(awk '/^\[package\]/{package=1;next} /^\[/{package=0} package && /^version = /{gsub(/"/,"",$3); print $3; exit}' "$root/Cargo.toml")
[ "$version" = "$manifest_version" ] || { echo 'VERSION and Cargo.toml disagree.' >&2; exit 1; }
case "$version" in *[!0-9.]*|'') echo 'Invalid release version.' >&2; exit 1;; esac
remote_version=$(awk '/^\[package\]/{package=1;next} /^\[/{package=0} package && /^version = /{gsub(/"/,"",$3); print $3; exit}' "$root/remote/Cargo.toml")
case "$remote_version" in *[!0-9.]*|'') echo 'Invalid companion version.' >&2; exit 1;; esac

mkdir -p -- "$output"
output=$(CDPATH= cd -- "$output" && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/clipbridge-package.XXXXXX")
trap 'rm -rf -- "$temporary"' EXIT
umask 077
hash_file() { shasum -a 256 "$1" | awk '{print $1}'; }

case "$target" in
    *-unknown-linux-musl)
        name="clipbridge-remote-$remote_version-$target"
        cp -- "$binary" "$temporary/$name"
        chmod 755 "$temporary/$name"
        mv -f -- "$temporary/$name" "$output/$name"
        printf '%s  %s\n' "$(hash_file "$output/$name")" "$name" > "$output/$name.sha256"
        printf '%s\n' "$output/$name"
        exit 0 ;;
esac

name="clipbridge-$version-$target"
stage="$temporary/$name"
mkdir "$stage"
files=(Cargo.toml Cargo.lock VERSION LICENSE README.md AGENTS.md src/README.md src/config.example.json docs/installation.md docs/usage.md docs/remote-images.md docs/plugins.md docs/planning/product-direction-and-distribution.md)
for relative in "${files[@]}"; do
    source="$root/$relative"
    [ -f "$source" ] && [ ! -L "$source" ] || { echo "Invalid package input: $relative" >&2; exit 1; }
    mkdir -p -- "$stage/$(dirname -- "$relative")"
    cp -- "$source" "$stage/$relative"
    chmod 644 "$stage/$relative"
done
cp -- "$binary" "$stage/clipbridge"
chmod 755 "$stage/clipbridge"
cat > "$stage/install.sh" <<'INSTALL'
#!/bin/sh
set -eu
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "$bundle_dir/clipbridge" install --source "$bundle_dir" "$@"
INSTALL
chmod 755 "$stage/install.sh"
printf '{"product":"clipbridge","schema":1,"version":"%s","target":"%s","binary_sha256":"%s"}\n' \
    "$version" "$target" "$(hash_file "$stage/clipbridge")" > "$stage/bundle.json"
chmod 644 "$stage/bundle.json"

# Store regular files only, with stable ordering and metadata on BSD and GNU tar.
(
    cd "$temporary"
    LC_ALL=C find "$name" -type f -print | LC_ALL=C sort > "$temporary/files.txt"
    while IFS= read -r file; do TZ=UTC touch -t 197001010000 "$file"; done < "$temporary/files.txt"
    if tar --version | head -n 1 | grep -q bsdtar; then
        COPYFILE_DISABLE=1 tar --format ustar --uid 0 --gid 0 --uname root --gname root \
            --no-acls --no-xattrs -cf "$temporary/archive.tar" -T "$temporary/files.txt"
    else
        tar --format ustar --owner=0 --group=0 --numeric-owner \
            -cf "$temporary/archive.tar" -T "$temporary/files.txt"
    fi
)
gzip -n -c "$temporary/archive.tar" > "$temporary/$name.tar.gz"
mv -f -- "$temporary/$name.tar.gz" "$output/$name.tar.gz"
printf '%s  %s\n' "$(hash_file "$output/$name.tar.gz")" "$name.tar.gz" > "$output/$name.tar.gz.sha256"
printf '%s\n' "$output/$name.tar.gz"
