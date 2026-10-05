use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Keep Unix socket paths below macOS's shorter sockaddr_un limit as well as Linux's.
        let root = tempfile::Builder::new()
            .prefix("cbr")
            .tempdir_in("/tmp")
            .unwrap();
        let state = root.path().join("state");
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let fixture = Self {
            _root: root,
            state,
            bin,
        };
        fixture.script("Xvfb", "#!/bin/sh\nexit 0\n");
        fixture.script("xauth", "#!/bin/sh\nexit 0\n");
        fixture.script(
            "xvfb-run",
            r#"#!/bin/sh
set -eu
umask 077
state="$CLIPBRIDGE_REMOTE_STATE_DIR"
printf 'start\n' >> "$state/starts"
while test "$#" -gt 0; do
    case "$1" in
        -a) shift ;;
        -n|-s|-e) shift 2 ;;
        *) break ;;
    esac
done
printf 'private cookie\n' > "$state/authority"
export DISPLAY=:90
export XAUTHORITY="$state/authority"
exec "$@"
"#,
        );
        fixture.script(
            "xclip",
            r#"#!/bin/sh
set -eu
state="$CLIPBRIDGE_REMOTE_STATE_DIR"
test "$DISPLAY" = ':90'
test "$XAUTHORITY" = "$state/authority"
test -z "${WAYLAND_DISPLAY-}"
case " $* " in
    *' -o '*)
        test -f "$state/selection.png"
        exec /bin/cat "$state/selection.png"
        ;;
esac
if test -f "$state/fail-next"; then
    /bin/rm "$state/fail-next"
    exit 7
fi
/bin/cat > "$state/selection.$$.png"
/bin/mv "$state/selection.$$.png" "$state/selection.png"
printf '%s\n' "$$" >> "$state/owners"
trap 'exit 0' TERM INT
while :; do /bin/sleep 0.1; done
"#,
        );
        fixture
    }

    fn script(&self, name: &str, text: &str) {
        let path = self.bin.join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clipbridge-remote"));
        command
            .env("CLIPBRIDGE_REMOTE_STATE_DIR", &self.state)
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("DISPLAY", ":12")
            .env("XAUTHORITY", "/caller/authority")
            .env("WAYLAND_DISPLAY", "wayland-personal");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn success(&self, args: &[&str]) -> String {
        let result = self.run(args);
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }

    fn image(&self, name: &str, content: &[u8]) -> PathBuf {
        let path = self._root.path().join(name);
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(content);
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Every fake server is isolated; cleanup never addresses the user's normal state path.
        let _ = self.command().arg("stop").output();
    }
}

#[test]
fn daemon_is_idempotent_and_supports_repeated_image_reads_and_stop() {
    let fixture = Fixture::new();
    assert!(fixture.success(&["status"]).contains("stopped"));
    assert!(!fixture.state.exists());
    assert!(fixture.success(&["ensure"]).contains("not yet published"));
    fixture.success(&["ensure"]);
    assert_eq!(
        fs::read_to_string(fixture.state.join("starts")).unwrap(),
        "start\n"
    );
    assert_eq!(
        fs::metadata(&fixture.state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(fixture.state.join("control.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let png = fixture.image("a ';$(echo injected).png", b"first image");
    assert_eq!(
        fixture.success(&["publish", png.to_str().unwrap()]),
        "Image ready on the remote clipboard.\n"
    );
    for _ in 0..3 {
        let output = fixture.run(&[
            "run",
            "--",
            "xclip",
            "-selection",
            "clipboard",
            "-target",
            "image/png",
            "-o",
        ]);
        assert!(output.status.success());
        assert_eq!(output.stdout, fs::read(&png).unwrap());
    }
    let next = fixture.image("next.png", b"second image");
    fixture.success(&["publish", next.to_str().unwrap()]);
    assert_eq!(
        fs::read(fixture.state.join("selection.png")).unwrap(),
        fs::read(next).unwrap()
    );
    assert!(fixture.success(&["status"]).contains("image ready"));
    fixture.success(&["stop"]);
    assert!(!fixture.state.join("control.sock").exists());
    for pid in fs::read_to_string(fixture.state.join("owners"))
        .unwrap()
        .lines()
    {
        assert!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid.parse().unwrap()), None).is_err(),
            "supervised clipboard owner {pid} survived stop"
        );
    }
    assert!(png.exists());
    assert!(fixture.success(&["stop"]).contains("already stopped"));
}

#[test]
fn run_executes_argv_without_shell_expansion_and_only_changes_child_environment() {
    let fixture = Fixture::new();
    let result = fixture.run(&["run", "--", "sh", "-c",
        "printf '%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" \"${WAYLAND_DISPLAY-unset}\" \"$1\" \"$2\"; exit 19",
        "sh", "argument with spaces", "$(touch should-not-exist)"]);
    assert_eq!(
        result.status.code(),
        Some(19),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let expected = format!(
        ":90\n{}/authority\nunset\nargument with spaces\n$(touch should-not-exist)\n",
        fixture.state.display()
    );
    assert_eq!(String::from_utf8(result.stdout).unwrap(), expected);
}

#[test]
fn failed_publication_reports_failure_and_restores_previous_image() {
    let fixture = Fixture::new();
    let old = fixture.image("previous.png", b"previous image");
    fixture.success(&["publish", old.to_str().unwrap()]);
    fs::write(fixture.state.join("fail-next"), b"yes").unwrap();
    let new = fixture.image("new.png", b"unpublishable");
    let result = fixture.run(&["publish", new.to_str().unwrap()]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert_eq!(
        fs::read(fixture.state.join("selection.png")).unwrap(),
        fs::read(old).unwrap()
    );
    assert!(fixture.success(&["status"]).contains("image ready"));
}

#[test]
fn refuses_symlink_or_public_state_without_modifying_it() {
    let fixture = Fixture::new();
    let elsewhere = fixture._root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, &fixture.state).unwrap();
    assert!(!fixture.run(&["ensure"]).status.success());
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    fs::remove_file(&fixture.state).unwrap();
    fs::create_dir(&fixture.state).unwrap();
    fs::set_permissions(&fixture.state, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!fixture.run(&["ensure"]).status.success());
    assert_eq!(
        fs::metadata(&fixture.state).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn doctor_and_invalid_png_do_not_start_a_server() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .success(&["doctor"])
            .contains("dependencies are available")
    );
    let path = fixture._root.path().join("invalid.png");
    fs::write(&path, b"text").unwrap();
    assert!(
        !fixture
            .run(&["publish", path.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!fixture.state.exists());
    let result = fixture
        .command()
        .env("PATH", fixture._root.path())
        .arg("doctor")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("missing remote tools"));
}

#[test]
fn concurrent_ensure_requests_start_only_one_server() {
    let fixture = Fixture::new();
    let children: Vec<_> = (0..4)
        .map(|_| {
            fixture
                .command()
                .arg("ensure")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.state.join("starts")).unwrap(),
        "start\n"
    );
}

#[test]
fn malformed_socket_request_is_rejected_without_stopping_server() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    let fixture = Fixture::new();
    fixture.success(&["ensure"]);
    let mut stream = UnixStream::connect(fixture.state.join("control.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"{\"command\":\"publish\",\"path\":\"/not-present\",\"extra\":true}\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.contains("\"ok\":false"));
    assert!(response.contains("unknown field"));
    assert!(fixture.success(&["status"]).contains("running"));
}

#[test]
fn stale_owned_socket_is_reported_stopped_and_stop_removes_it_under_locks() {
    let fixture = Fixture::new();
    fs::create_dir(&fixture.state).unwrap();
    fs::set_permissions(&fixture.state, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = fixture.state.join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    drop(listener);
    assert!(fixture.success(&["status"]).contains("stopped"));
    assert!(socket.exists(), "status should not remove stale files");
    assert!(fixture.success(&["stop"]).contains("already stopped"));
    assert!(!socket.exists());
    assert!(!fixture.state.join("starts").exists());
    fixture.success(&["ensure"]);
    assert!(fixture.success(&["status"]).contains("running"));
    assert!(socket.exists(), "live daemon must retain its socket");
}

#[test]
fn stop_refuses_socket_cleanup_when_server_lock_is_held_or_listener_is_live() {
    use std::os::unix::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    fs::create_dir(&fixture.state).unwrap();
    fs::set_permissions(&fixture.state, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = fixture.state.join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let result = fixture.run(&["stop"]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("live listener without the server lock")
    );
    assert!(socket.exists());
    drop(listener);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(fixture.state.join("server.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(!fixture.run(&["stop"]).status.success());
    assert!(socket.exists(), "locked server state must never be removed");
    drop(lock);
    fixture.success(&["stop"]);
    assert!(!socket.exists());
}

#[test]
fn startup_timeout_kills_descendants_after_launcher_has_exited() {
    check_failed_startup_cleanup("wait", "startup timeout");
}

#[test]
fn early_launcher_exit_kills_surviving_descendants() {
    check_failed_startup_cleanup(
        "while test ! -f \"$CLIPBRIDGE_REMOTE_STATE_DIR/stubborn.pid\"; do /bin/sleep 0.01; done; exit 7",
        "server exited",
    );
}

fn check_failed_startup_cleanup(launcher_end: &str, expected_error: &str) {
    let fixture = Fixture::new();
    fixture.script("xvfb-run", &format!(r#"#!/bin/sh
/bin/sh -c 'trap "" TERM; printf "%s\n" "$$" > "$CLIPBRIDGE_REMOTE_STATE_DIR/stubborn.pid"; while :; do /bin/sleep 1; done' &
{launcher_end}
"#));
    let result = fixture.run(&["ensure"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains(expected_error));
    let pid = fs::read_to_string(fixture.state.join("stubborn.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(pid);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if nix::sys::signal::kill(pid, None).is_err() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
            panic!("startup timeout left descendant {pid} alive");
        }
        std::thread::yield_now();
    }
}
