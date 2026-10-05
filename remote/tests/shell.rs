use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cbr shell 'quoted' ")
            .tempdir()
            .unwrap();
        fs::create_dir_all(root.path().join(".local/bin")).unwrap();
        fs::create_dir(root.path().join("bin")).unwrap();
        let fixture = Self { root };
        fixture.script(".local/bin/clipbridge-remote", "#!/bin/sh\nset -eu\ntest \"$1\" = run\ntest \"$2\" = --\nshift 2\nexport DISPLAY=:99 XAUTHORITY=/private/cookie\nunset WAYLAND_DISPLAY\nexec \"$@\"\n");
        fixture.script("bin/codex", "#!/bin/sh\nprintf 'child:%s:%s:%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" \"${WAYLAND_DISPLAY-unset}\"\nprintf 'arg:%s\\n' \"$@\"\nexit 23\n");
        fixture.script(
            "bin/claude",
            "#!/bin/sh\nprintf 'claude:%s\\n' \"$@\"\nexit 29\n",
        );
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.path(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clipbridge-remote"));
        command
            .env("HOME", self.root.path())
            .env("SHELL", "/bin/zsh")
            .env_remove("ZDOTDIR")
            .env("CLIPBRIDGE_REMOTE_STATE_DIR", self.path("never-started"));
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn success(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn shell(&self, shell: &str, script: &str, interactive: bool) -> Output {
        let program = if shell == "bash" {
            "/bin/bash"
        } else {
            "/bin/zsh"
        };
        let mut command = Command::new(program);
        if shell == "bash" {
            command.args(["--noprofile", "--norc"]);
        } else {
            command.arg("-f");
        }
        if interactive {
            command.arg("-i");
        }
        command
            .arg("-c")
            .arg(script)
            .env("HOME", self.root.path())
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("DISPLAY", ":12")
            .env("XAUTHORITY", "/original/cookie")
            .env("WAYLAND_DISPLAY", "wayland-personal")
            .env_remove("ZDOTDIR")
            .env_remove("_CLIPBRIDGE_CODEX_WRAPPED")
            .env_remove("_CLIPBRIDGE_CLAUDE_WRAPPED")
            .output()
            .unwrap()
    }

    fn snippet(&self) -> PathBuf {
        self.path(".local/share/clipbridge-remote/shell/init.sh")
    }
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

#[test]
fn wrappers_preserve_arguments_status_parent_environment_and_return_to_bash_and_zsh() {
    let fixture = Fixture::new();
    fixture.success(&["shell", "install"]);
    for shell in ["bash", "zsh"] {
        let script = format!(
            ". {}; . {}; codex 'two words' 'literal $(false)' \"quote'\\\"\"; printf 'status:%s\\n' \"$?\"; claude 'hello world'; printf 'status:%s\\n' \"$?\"; printf 'parent:%s:%s:%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" \"$WAYLAND_DISPLAY\"",
            quote(&fixture.snippet()),
            quote(&fixture.snippet())
        );
        let output = fixture.shell(shell, &script, true);
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "child::99:/private/cookie:unset\narg:two words\narg:literal $(false)\narg:quote'\"\nstatus:23\nclaude:hello world\nstatus:29\nparent::12:/original/cookie:wayland-personal\n",
            "{shell}"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("ClipBridge:"));
    }
    assert!(!fixture.path("never-started").exists());
}

#[test]
fn existing_aliases_and_functions_are_preserved_and_noninteractive_source_is_inert() {
    let fixture = Fixture::new();
    fixture.success(&["shell", "install"]);
    for shell in ["bash", "zsh"] {
        let script = format!(
            "alias codex='printf alias-kept'; function claude {{ printf function-kept; }}; . {}; alias codex; claude; printf '\\n';",
            quote(&fixture.snippet())
        );
        let output = fixture.shell(shell, &script, true);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("alias-kept"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("function-kept"));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("existing codex alias/function"));
        assert!(stderr.contains("existing claude alias/function"));
        let script = format!(
            ". {}; typeset -f codex >/dev/null && exit 9; typeset -f claude >/dev/null && exit 9; exit 0",
            quote(&fixture.snippet())
        );
        assert!(fixture.shell(shell, &script, false).status.success());
    }
}

#[test]
fn install_and_uninstall_preserve_exact_user_bytes_permissions_edits_and_backups() {
    let fixture = Fixture::new();
    let rc = fixture.path(".zshrc");
    let original = b"# user configuration\nexport EXAMPLE=1\n# no final newline";
    fs::write(&rc, original).unwrap();
    fs::set_permissions(&rc, fs::Permissions::from_mode(0o640)).unwrap();
    let first = fixture.success(&["shell", "install"]);
    assert!(first.contains("backup-"));
    let installed = fs::read(&rc).unwrap();
    fixture.success(&["shell", "install"]);
    assert_eq!(fs::read(&rc).unwrap(), installed);
    assert_eq!(
        fs::metadata(&rc).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let addition = b"\n# user added this afterwards\n";
    let mut edited = installed;
    edited.extend_from_slice(addition);
    fs::write(&rc, &edited).unwrap();
    fixture.success(&["shell", "uninstall"]);
    let mut expected = original.to_vec();
    expected.extend_from_slice(addition);
    assert_eq!(fs::read(&rc).unwrap(), expected);
    assert_eq!(
        fs::metadata(&rc).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(!fixture.snippet().exists());
    fixture.success(&["shell", "uninstall"]);
    let backups: Vec<_> = fs::read_dir(fixture.snippet().parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
    fixture.success(&["shell", "install"]);
    fixture.success(&["shell", "uninstall"]);
    assert_eq!(fs::read(rc).unwrap(), expected);
}

#[test]
fn defaults_cover_bash_login_zdotdir_and_explicit_startup_files() {
    let fixture = Fixture::new();
    fs::write(fixture.path(".bash_login"), b"# active login\n").unwrap();
    fixture.success(&["shell", "install", "--shell", "bash"]);
    assert!(fixture.path(".bashrc").exists());
    assert!(
        fs::read_to_string(fixture.path(".bash_login"))
            .unwrap()
            .contains("ClipBridge")
    );
    assert!(!fixture.path(".profile").exists());
    fixture.success(&["shell", "uninstall", "--shell", "bash"]);
    assert!(!fixture.path(".bashrc").exists());
    assert_eq!(
        fs::read(fixture.path(".bash_login")).unwrap(),
        b"# active login\n"
    );
    fs::create_dir(fixture.path("zsh config 'quoted'")).unwrap();
    let output = fixture
        .command()
        .args(["shell", "install", "--shell", "zsh"])
        .env("ZDOTDIR", fixture.path("zsh config 'quoted'"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.path("zsh config 'quoted'/.zshrc").exists());
    assert!(!fixture.path(".zshrc").exists());
    let output = fixture
        .command()
        .args(["shell", "install"])
        .env("ZDOTDIR", "relative")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let custom = fixture.path("custom rc");
    fixture.success(&["shell", "install", "--rc-file", custom.to_str().unwrap()]);
    assert!(custom.exists());
    fixture.success(&["shell", "uninstall"]);
    assert!(!custom.exists());
    assert!(!fixture.path("zsh config 'quoted'/.zshrc").exists());
}

#[test]
fn hooks_activate_in_interactive_startup_files_without_starting_a_server() {
    let fixture = Fixture::new();
    fixture.success(&["shell", "install", "--shell", "bash"]);
    fixture.success(&["shell", "install", "--shell", "zsh"]);
    for (shell, rc) in [("bash", ".bashrc"), ("bash", ".profile"), ("zsh", ".zshrc")] {
        let script = format!(
            ". {}; typeset -f codex >/dev/null; typeset -f claude >/dev/null",
            quote(&fixture.path(rc))
        );
        assert!(fixture.shell(shell, &script, true).status.success());
        let script = format!(
            ". {}; typeset -f codex >/dev/null && exit 9; exit 0",
            quote(&fixture.path(rc))
        );
        assert!(fixture.shell(shell, &script, false).status.success());
    }
    assert!(!fixture.path("never-started").exists());
}

#[test]
fn generic_profile_remains_usable_in_interactive_dash() {
    let Some(dash) = ["/bin/dash", "/usr/bin/dash", "/opt/homebrew/bin/dash"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
    else {
        eprintln!("Dash is unavailable; skipping the optional POSIX-shell check");
        return;
    };
    let fixture = Fixture::new();
    fixture.success(&["shell", "install", "--shell", "bash"]);
    let output = Command::new(dash)
        .args([
            "-i",
            "-c",
            ". \"$HOME/.profile\"; printf 'shell_survived\\n'",
        ])
        .env("HOME", fixture.root.path())
        .env_remove("BASH_VERSION")
        .env_remove("ZSH_VERSION")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "shell_survived\n"
    );
    assert!(!fixture.path("never-started").exists());
}

#[test]
fn edited_managed_blocks_or_snippets_are_refused_without_changing_user_files() {
    for edit_snippet in [false, true] {
        let fixture = Fixture::new();
        fixture.success(&["shell", "install"]);
        let rc = fixture.path(".zshrc");
        let edited_path = if edit_snippet {
            fixture.snippet()
        } else {
            rc.clone()
        };
        let mut bytes = fs::read(&edited_path).unwrap();
        if edit_snippet {
            bytes.extend_from_slice(b"# edited\n");
        } else {
            bytes = String::from_utf8(bytes)
                .unwrap()
                .replace("case $-", "case MODIFIED")
                .into_bytes();
        }
        fs::write(&edited_path, &bytes).unwrap();
        let rc_before = fs::read(&rc).unwrap();
        for action in ["install", "uninstall"] {
            let output = fixture.run(&["shell", action]);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("modified"));
            assert_eq!(fs::read(&edited_path).unwrap(), bytes);
            assert_eq!(fs::read(&rc).unwrap(), rc_before);
        }
    }
}

#[test]
fn symlinks_untracked_hooks_and_unfamiliar_directories_are_refused() {
    for target in [
        ".zshrc",
        ".local/share/clipbridge-remote/shell",
        ".local/share",
    ] {
        let fixture = Fixture::new();
        let path = fixture.path(target);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let outside = fixture.path("outside");
        if target == ".zshrc" {
            fs::write(&outside, b"keep me").unwrap();
        } else {
            fs::create_dir(&outside).unwrap();
        }
        symlink(&outside, &path).unwrap();
        assert!(
            !fixture.run(&["shell", "install"]).status.success(),
            "{target}"
        );
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    let fixture = Fixture::new();
    fs::write(
        fixture.path(".zshrc"),
        b"# >>> ClipBridge remote shell >>>\nuser block\n",
    )
    .unwrap();
    assert!(!fixture.run(&["shell", "install"]).status.success());
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.snippet().parent().unwrap()).unwrap();
    fs::write(fixture.snippet(), b"unfamiliar").unwrap();
    assert!(!fixture.run(&["shell", "install"]).status.success());
    assert_eq!(fs::read(fixture.snippet()).unwrap(), b"unfamiliar");
}

#[test]
fn init_and_unsupported_shell_validation_do_not_write_files_or_start_services() {
    let fixture = Fixture::new();
    let output = fixture.success(&["shell", "init"]);
    assert!(output.contains("function codex"));
    assert!(!fixture.snippet().exists());
    let output = fixture
        .command()
        .args(["shell", "install"])
        .env("SHELL", "/bin/fish")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.snippet().exists());
    assert!(!fixture.path("never-started").exists());
}
