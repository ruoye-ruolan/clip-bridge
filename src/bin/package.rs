//! Create an allowlisted source distribution without personal or generated files.
use anyhow::{Context as _, Result, bail};
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

const RUST_FILES: &[&str] = &[
    "src/main.rs",
    "src/lib.rs",
    "src/common.rs",
    "src/config.rs",
    "src/clipboard.rs",
    "src/control.rs",
    "src/monitor.rs",
    "src/service.rs",
    "src/installer.rs",
    "src/remote.rs",
    "src/bin/package.rs",
    "tests/cli.rs",
    "tests/installation.rs",
    "remote/src/main.rs",
    "remote/src/shell.rs",
    "remote/tests/cli.rs",
    "remote/tests/shell.rs",
];

fn source_root() -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let root = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("run package from this repository with make package")?;
    if !root.join("Cargo.toml").is_file() {
        bail!("source manifest is missing");
    }
    Ok(root.to_owned())
}
fn collect(
    root: &Path,
    relative: &Path,
    extension: &str,
    files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!("refusing a symlink in package inputs: {}", path.display());
        }
        if kind.is_dir() {
            collect(root, &path, extension, files)?;
        } else if path.extension().is_some_and(|ext| ext == extension) {
            files.insert(path);
        }
    }
    Ok(())
}
fn package(root: &Path) -> Result<PathBuf> {
    let version = fs::read_to_string(root.join("VERSION"))?;
    if version.trim() != env!("CARGO_PKG_VERSION") {
        bail!("VERSION and Cargo.toml version disagree");
    }
    let name = format!("clipbridge-{}", version.trim());
    let mut files: BTreeSet<PathBuf> = [
        "Cargo.toml",
        "Cargo.lock",
        "VERSION",
        "LICENSE",
        "README.md",
        "AGENTS.md",
        "clipbridge",
        "install.sh",
        "Makefile",
        "src/Makefile",
        "src/README.md",
        "src/config.example.json",
        "remote/Cargo.toml",
        "remote/Cargo.lock",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    files.extend(RUST_FILES.iter().map(PathBuf::from));
    collect(root, Path::new("docs"), "md", &mut files)?;
    let dist = root.join("dist");
    fs::create_dir_all(&dist)?;
    let archive = dist.join(format!("{name}.tar.gz"));
    let temporary = tempfile::NamedTempFile::new_in(&dist)?;
    let compressed = GzBuilder::new()
        .mtime(0)
        .write(temporary, Compression::default());
    let mut builder = tar::Builder::new(compressed);
    for relative in files {
        let path = root.join(&relative);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() || !path.is_file() {
            bail!("invalid package file {}", path.display());
        }
        let mut file = File::open(&path)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(file.metadata()?.len());
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_mode(
            if matches!(relative.to_str(), Some("clipbridge" | "install.sh")) {
                0o755
            } else {
                0o644
            },
        );
        header.set_cksum();
        builder.append_data(&mut header, Path::new(&name).join(relative), &mut file)?;
    }
    let compressed = builder.into_inner()?;
    let temporary = compressed.finish()?;
    temporary.persist(&archive).map_err(|e| e.error)?;
    let checksum = format!("{:x}", Sha256::digest(fs::read(&archive)?));
    fs::write(
        archive.with_extension("gz.sha256"),
        format!("{checksum}  {name}.tar.gz\n"),
    )?;
    Ok(archive)
}
fn main() -> Result<()> {
    let archive = package(&source_root()?)?;
    println!("{}", archive.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn package_excludes_config_caches_and_old_languages() {
        let dir = tempfile::tempdir().unwrap();
        for folder in ["src/bin", "src/build", "tests", "docs"] {
            fs::create_dir_all(dir.path().join(folder)).unwrap();
        }
        for name in [
            "Cargo.toml",
            "Cargo.lock",
            "LICENSE",
            "README.md",
            "AGENTS.md",
            "clipbridge",
            "install.sh",
            "Makefile",
            "src/Makefile",
            "src/README.md",
            "src/config.example.json",
            "remote/Cargo.toml",
            "remote/Cargo.lock",
        ] {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        fs::write(dir.path().join("VERSION"), env!("CARGO_PKG_VERSION")).unwrap();
        for name in RUST_FILES.iter().copied().chain(["docs/usage.md"]) {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        for name in [
            "src/config.json",
            "src/build/private",
            "src/build/generated.rs",
            "src/old.py",
            "src/old.swift",
        ] {
            fs::write(dir.path().join(name), b"PRIVATE").unwrap();
        }
        let archive = package(dir.path()).unwrap();
        let reader = flate2::read::GzDecoder::new(File::open(&archive).unwrap());
        let mut archive = tar::Archive::new(reader);
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().into_owned();
            let mut bytes = vec![];
            entry.read_to_end(&mut bytes).unwrap();
            assert_ne!(bytes, b"PRIVATE");
            names.push(path);
        }
        assert!(names.iter().any(|p| p.ends_with("src/main.rs")));
        assert!(!names.iter().any(|p| p.ends_with("src/config.json")));
    }
}
