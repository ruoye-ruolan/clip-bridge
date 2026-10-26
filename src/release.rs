//! Versioned native release assets. Network sources are fixed to this project's releases.
use crate::common::{CommandSpec, Context, VERSION};
use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const RELEASE_ROOT: &str = "https://github.com/ruoye-ruolan/clip-bridge/releases/download";
const MAX_BINARY: u64 = 64 * 1024 * 1024;
const MAX_METADATA: u64 = 16 * 1024;
const REMOTE_MANIFEST: &str = include_str!("../remote/Cargo.toml");

/// The helper has an independent version, embedded from the same manifest as its sources.
pub fn companion_version() -> &'static str {
    REMOTE_MANIFEST
        .lines()
        .skip_while(|line| line.trim() != "[package]")
        .skip(1)
        .take_while(|line| !line.trim().starts_with('['))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "version")
                .then_some(value.trim())?
                .strip_prefix('"')?
                .strip_suffix('"')
        })
        .expect("embedded remote package manifest must define a quoted version")
}

pub fn remote_target(machine: &str) -> Result<&'static str> {
    match machine.trim() {
        "x86_64" | "amd64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Ok("aarch64-unknown-linux-musl"),
        machine => bail!(
            "No prebuilt Linux companion for architecture {machine}; use explicit source setup"
        ),
    }
}

pub fn remote_asset_name(target: &str) -> Result<String> {
    ensure!(
        matches!(
            target,
            "x86_64-unknown-linux-musl" | "aarch64-unknown-linux-musl"
        ),
        "unsupported Linux release target"
    );
    ensure!(
        valid_version(companion_version()),
        "invalid embedded companion version"
    );
    Ok(format!(
        "clipbridge-remote-{}-{target}",
        companion_version()
    ))
}

fn regular_file(path: &Path, limit: u64) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open release file {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "release input is not a regular file: {}",
        path.display()
    );
    ensure!(
        metadata.len() > 0 && metadata.len() <= limit,
        "release file has invalid size: {}",
        path.display()
    );
    Ok(file)
}

fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    regular_file(path, limit)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= limit,
        "release file changed size while reading"
    );
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn checksum(bytes: &[u8], name: &str) -> Result<String> {
    let text = std::str::from_utf8(bytes).context("release checksum is not UTF-8")?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let (hash, filename) = text
        .split_once("  ")
        .context("invalid release checksum format")?;
    ensure!(
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) && filename == name,
        "release checksum must contain exactly the requested asset name and one SHA-256 digest"
    );
    Ok(hash.to_ascii_lowercase())
}

fn fetch(ctx: &Context, name: &str, output: &Path, limit: u64) -> Result<()> {
    ensure!(valid_version(VERSION), "invalid compiled release version");
    let url = format!("{RELEASE_ROOT}/v{VERSION}/{name}");
    ctx.exec(
        CommandSpec::new("/usr/bin/curl")
            .args([
                "--fail", "--silent", "--show-error", "--location",
                "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2",
                "--connect-timeout", "15", "--max-time", "180", "--max-filesize",
            ])
            .args([limit.to_string()])
            .args(["--output"])
            .args([output.as_os_str()])
            .args([url])
            .timeout(190),
    ).context("Could not download the versioned companion release; use --assets-dir for offline assets or explicit --build-from-source setup")?;
    Ok(())
}

/// Fetch only this release's official companion, or verify explicitly supplied offline assets.
pub fn download_companion(
    ctx: &Context,
    target: &str,
    assets_dir: Option<&Path>,
) -> Result<tempfile::NamedTempFile> {
    let name = remote_asset_name(target)?;
    let (binary, expected) = if let Some(directory) = assets_dir {
        ensure!(directory.is_dir(), "release asset directory does not exist");
        (
            read_regular(&directory.join(&name), MAX_BINARY)?,
            checksum(
                &read_regular(&directory.join(format!("{name}.sha256")), MAX_METADATA)?,
                &name,
            )?,
        )
    } else {
        let binary = tempfile::NamedTempFile::new()?;
        let checksum_file = tempfile::NamedTempFile::new()?;
        fetch(
            ctx,
            &format!("{name}.sha256"),
            checksum_file.path(),
            MAX_METADATA,
        )?;
        let expected = checksum(&read_regular(checksum_file.path(), MAX_METADATA)?, &name)?;
        fetch(ctx, &name, binary.path(), MAX_BINARY)?;
        (read_regular(binary.path(), MAX_BINARY)?, expected)
    };
    ensure!(
        digest(&binary) == expected,
        "companion SHA-256 checksum mismatch; nothing was installed"
    );
    let mut verified = tempfile::NamedTempFile::new()?;
    verified.write_all(&binary)?;
    verified.as_file().sync_all()?;
    Ok(verified)
}

fn valid_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeBundle {
    product: String,
    schema: u32,
    version: String,
    target: String,
    binary_sha256: String,
}

/// Validate a native upgrade before executing its binary. Its version may be newer than ours.
pub fn validate_native_bundle(path: &Path) -> Result<()> {
    let root = fs::symlink_metadata(path).context("inspect native bundle")?;
    ensure!(
        root.is_dir() && !root.file_type().is_symlink(),
        "native bundle must be a real directory"
    );
    let manifest: NativeBundle =
        serde_json::from_slice(&read_regular(&path.join("bundle.json"), MAX_METADATA)?)
            .context("invalid native bundle manifest")?;
    ensure!(
        manifest.product == "clipbridge" && manifest.schema == 1,
        "unrecognized native bundle"
    );
    ensure!(
        valid_version(&manifest.version),
        "invalid native bundle version"
    );
    let expected_target = match std::env::consts::ARCH {
        "aarch64" => "aarch64-apple-darwin",
        "x86_64" => "x86_64-apple-darwin",
        _ => bail!("No native Mac distribution for this architecture"),
    };
    ensure!(
        cfg!(target_os = "macos") && manifest.target == expected_target,
        "native bundle target does not match this Mac"
    );
    ensure!(
        std::str::from_utf8(&read_regular(&path.join("VERSION"), 64)?)?.trim() == manifest.version,
        "native bundle VERSION disagrees with its manifest"
    );
    let binary = read_regular(&path.join("clipbridge"), MAX_BINARY)?;
    ensure!(
        manifest.binary_sha256.len() == 64
            && manifest
                .binary_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "invalid native binary digest"
    );
    ensure!(
        digest(&binary) == manifest.binary_sha256.to_ascii_lowercase(),
        "native bundle binary checksum mismatch"
    );
    // Keep the compiled installer's existing manifest fingerprint and payload checks useful.
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "LICENSE",
        "src/config.example.json",
    ] {
        if let Some(parent) = Path::new(name)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            let metadata = fs::symlink_metadata(path.join(parent))?;
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "native bundle payload directory is not a real directory"
            );
        }
        drop(read_regular(&path.join(name), 4 * 1024 * 1024)?);
    }
    Ok(())
}
