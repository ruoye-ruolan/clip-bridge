"""Upload new clipboard images while a matching SSH session exists (macOS)."""
import argparse
import atexit
import concurrent.futures
import fcntl
import json
import logging
import os
from pathlib import Path
import selectors
import shlex
import shutil
import subprocess
import tempfile
import uuid

from configuration import DEFAULT_CONFIG, LEGACY_CONFIG, SSH_OPTIONS, load_config

HOME = Path.home()
ROOT = Path(__file__).resolve().parent
BASE = ROOT / 'build'
CACHE = HOME / 'Library/Caches/clipbridge/auto'
REMOTE = None


def run(args, timeout=10):
    return subprocess.run(args, capture_output=True, text=True, timeout=timeout, check=True)


def is_session(args, host):
    if not args or Path(args[0]).name != 'ssh':
        return False
    i = 1
    takes_value = set('BbcDEeFIiJLlmOoPpQRSWw')
    while i < len(args) and args[i].startswith('-'):
        option = args[i]
        if option == '--':
            i += 1
            break
        if option in ('-G', '-V') or option.startswith(('-O', '-Q')):
            return False
        if len(option) == 2 and option[1] in takes_value:
            i += 2
        else:
            i += 1
    return args[i:] == [host]


def connected(host):
    try:
        listing = run(['/bin/ps', '-axo', 'pid=,uid=,args=']).stdout
        pids = []
        for line in listing.splitlines():
            fields = line.strip().split(None, 2)
            if len(fields) != 3 or fields[1] != str(os.getuid()):
                continue
            try:
                args = shlex.split(fields[2])
            except ValueError:
                continue
            if is_session(args, host):
                pids.append(fields[0])
        if not pids:
            return False
        result = run(['/usr/sbin/lsof', '-nP', '-a', '-p', ','.join(pids), '-iTCP', '-sTCP:ESTABLISHED', '-F', 'n'])
        return any(line.startswith('n') and '->' in line for line in result.stdout.splitlines())
    except (subprocess.SubprocessError, OSError):
        return False


def notify(message):
    script = 'on run argv\n display notification (item 1 of argv) with title "ClipBridge"\nend run'
    try:
        run(['/usr/bin/osascript', '-e', script, message])
    except (subprocess.SubprocessError, OSError):
        logging.exception('Could not show notification')


def upload(path, host, captured_count=None):
    # Recheck queued jobs: a session may close before a previous upload finishes.
    if not connected(host):
        logging.info('Skipped after disconnect: %s', path.name)
        if captured_count is not None:
            path.unlink(missing_ok=True)
        return
    tempdir = Path(tempfile.mkdtemp(prefix='upload-', dir=CACHE))
    local = tempdir / ('shot-' + str(uuid.uuid4()) + path.suffix.lower())
    remote = REMOTE.rstrip('/') + '/' + local.name
    try:
        count = captured_count if captured_count is not None else run([str(BASE / 'clipboard-path'), 'count']).stdout.strip()
        if path.is_symlink():
            raise ValueError('Symlink rejected')
        shutil.copyfile(path, local)
        if captured_count is not None:
            path.unlink(missing_ok=True)
        # Validate that the saved file is an image before sending it.
        run(['/usr/bin/sips', '-g', 'format', str(local)])
        run(['/usr/bin/ssh'] + SSH_OPTIONS + [host, "umask 077; mkdir -p -- " + shlex.quote(REMOTE)], 35)
        run(['/usr/bin/scp', '-q'] + SSH_OPTIONS + [str(local), host + ':' + remote], 120)
        result = run([str(BASE / 'clipboard-path'), 'set-if', count, remote])
        logging.info('Uploaded %s -> %s; %s', path.name, remote, result.stdout.strip())
        shutil.rmtree(tempdir)
        notify('Uploaded; remote path copied.' if result.stdout.strip() == 'copied' else 'Uploaded; newer clipboard preserved. See the log for the path.')
    except (subprocess.SubprocessError, OSError, ValueError):
        logging.exception('Upload failed; retained at %s', local)
        notify('Upload failed; local image retained. See the ClipBridge log.')


def report_upload_result(future):
    try:
        future.result()
    except Exception:
        logging.exception('Unexpected upload worker failure; inspect the local cache')


def process_event(event, host, executor):
    if not event.get('image') or not connected(host):
        return
    count = str(int(event['count']))
    path = CACHE / ('clipboard-' + str(uuid.uuid4()) + '.png')
    try:
        run([str(BASE / 'clipboard-watch'), 'capture', count, str(path)])
    except subprocess.CalledProcessError as error:
        path.unlink(missing_ok=True)
        if error.returncode not in (3, 4):
            logging.exception('Could not capture clipboard image')
        return
    future = executor.submit(upload, path, host, count)
    future.add_done_callback(report_upload_result)


def monitor_events(watcher, host, executor, control):
    """Poll both helper output and cooperative shutdown, including while idle."""
    pending = b''
    with selectors.DefaultSelector() as selector:
        selector.register(watcher.stdout, selectors.EVENT_READ)
        while not control.handoff_requested():
            if not selector.select(timeout=0.3):
                continue
            data = os.read(watcher.stdout.fileno(), 4096)
            if not data:
                raise RuntimeError('Clipboard watcher exited unexpectedly')
            pending += data
            while b'\n' in pending:
                line, pending = pending.split(b'\n', 1)
                if control.handoff_requested():
                    return
                process_event(json.loads(line), host, executor)


def main(argv=None):
    from monitor_control import monitor_registration

    global REMOTE
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, default=(
        DEFAULT_CONFIG if DEFAULT_CONFIG.exists() or not LEGACY_CONFIG.exists() else LEGACY_CONFIG
    ))
    parser.add_argument('--foreground', action='store_true',
                        help='allow start to request a transition to background monitoring')
    args = parser.parse_args(argv)
    try:
        host, REMOTE = load_config(args.config)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    for helper in ('clipboard-watch', 'clipboard-path'):
        if not os.access(BASE / helper, os.X_OK):
            parser.error('Missing compiled helpers; run make -C prototype/macos build')
    os.umask(0o077)
    CACHE.mkdir(parents=True, exist_ok=True)
    # Keep the lock until the watcher and all queued uploads finish.
    lock = (CACHE / 'watcher.lock').open('a')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        lock.close()
        parser.error('Another ClipBridge prototype instance is already running')
    # ThreadPoolExecutor joins unfinished workers before Python's atexit handlers.
    # Retain this descriptor if Ctrl+C interrupts the join, so a replacement cannot
    # upload alongside workers that are still finishing during interpreter exit.
    release_lock = lock.close
    atexit.register(release_lock)
    logging.basicConfig(level=logging.INFO, format='%(asctime)s %(levelname)s %(message)s')
    logging.info('Watching new clipboard images; requires established ssh %s session', host)
    try:
        with monitor_registration(CACHE, args.config, foreground=args.foreground) as control:
            # Startup baselines existing clipboard content; use binary unbuffered reads
            # so selector readiness also works when multiple events arrive together.
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
                with subprocess.Popen([str(BASE / 'clipboard-watch'), 'watch'],
                                      stdout=subprocess.PIPE, bufsize=0) as watcher:
                    try:
                        monitor_events(watcher, host, executor, control)
                        logging.info('Switching to background mode; finishing queued uploads first')
                    finally:
                        if watcher.poll() is None:
                            watcher.terminate()
                        watcher.wait(timeout=5)
    except KeyboardInterrupt:
        logging.info('Stopping monitor; unfinished uploads may delay process exit')
    else:
        release_lock()
        atexit.unregister(release_lock)


if __name__ == '__main__':
    main()
