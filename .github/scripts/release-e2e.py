#!/usr/bin/env python3
"""Release e2e on the runner's own OS: install → exercise → auto-update → uninstall,
then upgrade-from-the-previous-release → rollback → uninstall.

Runs against the exact installer the release will publish (the build job's artifact),
installed the way a user installs it:

  windows  NSIS setup `.exe /S`; found via the Uninstall registry key; removed with its
           registered uninstaller.
  macos    DMG mounted and the app copied to /Applications (the previous release, which
           may predate DMGs, comes from its updater `.app.tar.gz`); removed by deleting it.
  linux    AppImage copied to ~/Applications and run through FUSE; removed by deleting it.

Scenarios
  1. fresh      install the new build on a clean machine; check the installed identity
                (version, signature on macOS); exercise it; uninstall; assert nothing is
                left behind.
     auto-update  still installed, run it with its updater pointed at a local server that
                offers a newer version whose download is THIS release's own signed
                update artifact (the exact `latest.json` entry the release publishes,
                re-hosted). The app must download and verify it, quit cleanly, install
                it with the OS's real mechanism (NSIS passive installer on Windows, bundle
                swap on macOS, AppImage swap on Linux), and come back up as a new process
                on the replaced files without reporting a crash.
     schedule     the update settings (Settings → Updates), driven through the real
                config file: a release that appears while the app is running is
                found by a scheduled re-check (no restart) and NOT installed on its
                own; with "Check for updates automatically" off the app never
                contacts the server; with "Install updates automatically" on it
                downloads in the background, installs nothing mid-session, and
                installs when the user quits, without restarting the app.
  2. upgrade    install the PREVIOUS published release; check that it detects this release
                as an update (its updater reads the new `latest.json`, served locally).
                If that release can install updates unattended, let its own updater
                perform the upgrade (as above); otherwise install the new build over it.
                Either way check exactly one install remains at the new version and it
                runs with the old version's config.
  3. rollback   uninstall the new build, reinstall the previous one, and check it still
                runs against the config the new version left behind (the path a user takes
                back after a bad release); uninstall.

"Exercise" = headless renders of real views (GPU, fonts, layout) with a library scanned
from generated tagged tracks, then the real windowed app autoplaying that library for a
while: it must stay up, report the expected version in its log, start playback, never
panic, and (Unix) quit cleanly on SIGTERM so the next launch sees no crash.

Usage:
  release-e2e.py --installer PATH --version X.Y.Z --target-key KEY \
                 [--manifest new-latest.json] [--update-meta KEY.meta.json] \
                 [--previous-manifest URL] [--out DIR]

`--update-meta` is the build's updater sidecar (artifact name + signature; the artifact
sits next to it). Without it the auto-update scenario is skipped. `SOUL_E2E_APPS_DIR`
overrides /Applications on macOS (for running this on a developer machine).
"""

from __future__ import annotations

import argparse
import faulthandler
import functools
import http.server
import json
import os
import re
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import zlib
from pathlib import Path

# Windows consoles default to cp1252; the report uses arrows and dashes.
for _stream in (sys.stdout, sys.stderr):
    _stream.reconfigure(encoding="utf-8", errors="replace")

IS_WIN = sys.platform == "win32"
IS_MAC = sys.platform == "darwin"
APP_NAME = "Soul Player"
BUNDLE_ID = "co.soulaudio.soulplayer"
APPS_DIR = Path(os.environ.get("SOUL_E2E_APPS_DIR", "/Applications"))


# ---------------------------------------------------------------- reporting


class E2EFailure(Exception):
    pass


def log(msg: str) -> None:
    print(msg, flush=True)


def ok(msg: str) -> None:
    log(f"  ok: {msg}")


def check(cond: bool, msg: str) -> None:
    if not cond:
        raise E2EFailure(msg)


def section(title: str) -> None:
    log(f"\n::group::{title}" if os.environ.get("GITHUB_ACTIONS") else f"\n== {title}")


def end_section() -> None:
    if os.environ.get("GITHUB_ACTIONS"):
        log("::endgroup::")


def run(cmd: list[str], timeout: int = 300, **kw) -> subprocess.CompletedProcess:
    log(f"  $ {' '.join(map(str, cmd))}")
    return subprocess.run(cmd, timeout=timeout, check=True, **kw)


def semver(v: str) -> tuple[int, ...]:
    return tuple(int(x) for x in re.findall(r"\d+", v.split("-")[0])[:3])


def wait_until(pred, timeout: float, step: float = 1.0) -> bool:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if pred():
            return True
        time.sleep(step)
    return pred()


# ---------------------------------------------------------------- fixtures


def write_fixture_library(d: Path) -> None:
    """Three short silent tagged WAVs (two rates) in one album folder, plus a cover."""
    d.mkdir(parents=True, exist_ok=True)

    def info(tags):
        body = b"INFO"
        for k, v in tags:
            b = v.encode() + b"\0"
            if len(b) % 2:
                b += b"\0"
            body += k.encode() + struct.pack("<I", len(v) + 1) + b
        return b"LIST" + struct.pack("<I", len(body)) + body

    def wav(name, secs, rate, title):
        data = b"\0" * (int(secs * rate) * 4)
        fmt = b"fmt " + struct.pack("<IHHIIHH", 16, 1, 2, rate, rate * 4, 4, 16)
        tags = info([("INAM", title), ("IART", "Release E2E"), ("IPRD", "Release E2E Album")])
        riff = b"WAVE" + fmt + tags + b"data" + struct.pack("<I", len(data)) + data
        (d / name).write_bytes(b"RIFF" + struct.pack("<I", len(riff)) + riff)

    wav("01 - One.wav", 40, 44100, "E2E One")
    wav("02 - Two.wav", 40, 96000, "E2E Two")
    wav("03 - Three.wav", 40, 48000, "E2E Three")


def png_check(path: Path, want_w: int, want_h: int) -> str:
    """A real, non-blank PNG of (at least) the requested logical size."""
    check(path.is_file() and path.stat().st_size > 0, f"no frame written to {path}")
    raw = path.read_bytes()
    check(raw[:8] == b"\x89PNG\r\n\x1a\n", f"{path} is not a PNG")
    w, h = struct.unpack(">II", raw[16:24])
    check(w >= want_w and h >= want_h, f"{path} is {w}x{h}, expected ≥ {want_w}x{want_h}")
    idat, i = b"", 8
    while i < len(raw):
        n = struct.unpack(">I", raw[i : i + 4])[0]
        kind = raw[i + 4 : i + 8]
        if kind == b"IDAT":
            idat += raw[i + 8 : i + 8 + n]
        i += 12 + n
    pixels = zlib.decompress(idat)
    distinct = len(set(pixels[:: max(1, len(pixels) // 200_000)]))
    check(distinct > 24, f"{path} looks blank ({distinct} distinct byte values)")
    return f"{w}x{h}, {distinct} distinct values"


# ---------------------------------------------------------------- the app's log


class AppEnv:
    """An isolated user profile for the app: HOME (macOS/Linux) and its log directory.

    Windows resolves its folders through the shell (not env vars), so there the runner's
    own profile is used, the runner is a throwaway VM anyway.
    """

    def __init__(self, root: Path, name: str):
        self.home = root / f"home-{name}"
        self.home.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ)
        env["SOUL_PLAYER_NO_UPDATE"] = "1"
        env["RUST_LOG"] = "info"
        if not IS_WIN:
            env["HOME"] = str(self.home)
            for k in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
                env.pop(k, None)
        if sys.platform.startswith("linux"):
            # Hosted runners have no sound card: route ALSA's default PCM to the null
            # device so the output path is exercised end to end.
            (self.home / ".asoundrc").write_text("pcm.!default { type null }\n")
        self.env = env

    @property
    def log_dir(self) -> Path:
        if IS_MAC:
            return self.home / "Library" / "Logs" / APP_NAME
        if IS_WIN:
            return Path(os.environ["LOCALAPPDATA"]) / "soul-player" / "logs"
        return self.home / ".local" / "state" / "soul-player" / "logs"

    def log_mark(self) -> dict[Path, int]:
        if not self.log_dir.is_dir():
            return {}
        return {p: p.stat().st_size for p in self.log_dir.glob("Soul*.log")}

    def log_since(self, mark: dict[Path, int]) -> str:
        if not self.log_dir.is_dir():
            return ""
        out = []
        for p in sorted(self.log_dir.glob("Soul*.log")):
            with open(p, "rb") as f:
                f.seek(mark.get(p, 0))
                out.append(f.read().decode("utf-8", "replace"))
        return "".join(out)


# ---------------------------------------------------------------- installs


class Install:
    exe: Path  # what to launch
    version: str  # what the OS records for it


class WindowsInstall(Install):
    KEYS = [
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ]

    @classmethod
    def entries(cls) -> list[dict]:
        import winreg  # noqa: PLC0415 (Windows only)

        found = []
        for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
            for key in cls.KEYS:
                try:
                    root = winreg.OpenKey(hive, key)
                except OSError:
                    continue
                for i in range(winreg.QueryInfoKey(root)[0]):
                    name = winreg.EnumKey(root, i)
                    with winreg.OpenKey(root, name) as k:

                        def val(n, k=k):
                            try:
                                return winreg.QueryValueEx(k, n)[0]
                            except OSError:
                                return ""

                        if val("DisplayName") == APP_NAME:
                            found.append(
                                {
                                    "key": f"{key}\\{name}",
                                    "version": val("DisplayVersion"),
                                    "location": val("InstallLocation"),
                                    "uninstall": val("UninstallString"),
                                }
                            )
        return found

    def __init__(self, entry: dict):
        loc = entry["location"].strip('"') or str(
            Path(entry["uninstall"].strip('"').split('"')[0]).parent
        )
        self.dir = Path(loc)
        self.entry = entry
        self.version = entry["version"]
        exes = [p for p in self.dir.glob("*.exe") if not p.name.lower().startswith("uninstall")]
        check(len(exes) == 1, f"expected one app exe in {self.dir}, found {exes}")
        self.exe = exes[0]


def has_diskutil_image() -> bool:
    """`diskutil image` (macOS 26+) replaces the deprecated `hdiutil attach`."""
    return subprocess.run(["diskutil", "image", "attach", "--help"], capture_output=True).returncode == 0


def attach_dmg(dmg: Path, mnt: Path) -> None:
    """Mount a DMG read-only, hidden from Finder, at `mnt`."""
    if has_diskutil_image():
        run(["diskutil", "image", "attach", "--readOnly", "--nobrowse", "--mountPoint", str(mnt), str(dmg)])
    else:  # macOS 15 (the hosted runners): no `diskutil image` yet
        run(["hdiutil", "attach", "-nobrowse", "-readonly", "-mountpoint", str(mnt), str(dmg)])


def eject_dmg(mnt: Path) -> None:
    """Unmount and detach the image (`diskutil eject` does both on every macOS)."""
    for _ in range(3):
        if subprocess.run(["diskutil", "eject", str(mnt)], capture_output=True).returncode == 0:
            return
        time.sleep(2)
    log(f"::warning::could not eject the DMG mounted at {mnt}")


def install(installer: Path, work: Path, current_build: bool = True) -> Install:
    log(f"  installing {installer.name}")
    if IS_WIN:
        run([str(installer), "/S"], timeout=600)
        entries = WindowsInstall.entries()
        check(len(entries) == 1, f"expected exactly one '{APP_NAME}' install, found {entries}")
        inst = WindowsInstall(entries[0])
    elif IS_MAC:
        app = APPS_DIR / f"{APP_NAME}.app"
        if app.exists():
            shutil.rmtree(app)  # what a drag-to-Applications "Replace" does
        if installer.name.endswith(".dmg"):
            mnt = Path(tempfile.mkdtemp(dir=work))
            attach_dmg(installer, mnt)
            try:
                check(os.readlink(mnt / "Applications") == "/Applications", "DMG lacks /Applications")
                run(["ditto", str(mnt / f"{APP_NAME}.app"), str(app)])
            finally:
                eject_dmg(mnt)
        else:  # the updater's .app.tar.gz
            run(["tar", "-xzf", str(installer), "-C", str(APPS_DIR)])
        check(app.is_dir(), f"{app} missing after install")
        inst = Install()
        inst.app = app
        plist = app / "Contents" / "Info.plist"

        def pb(k):
            return subprocess.check_output(
                ["/usr/libexec/PlistBuddy", "-c", f"Print :{k}", str(plist)], text=True
            ).strip()

        # Only the build under test is held to the CURRENT bundle id. The
        # previously published release is installed here as an upgrade source,
        # and it legitimately carries whatever id it shipped with, asserting
        # today's id against yesterday's artifact fails every time the
        # identifier changes, which is exactly what happened when it became
        # co.soulaudio.soulplayer.
        if current_build:
            check(
                pb("CFBundleIdentifier") == BUNDLE_ID,
                f"bundle id {pb('CFBundleIdentifier')}",
            )
        inst.version = pb("CFBundleShortVersionString")
        inst.exe = app / "Contents" / "MacOS" / pb("CFBundleExecutable")
        if current_build:  # releases before 0.4 carry only the linker's signature
            run(["codesign", "--verify", "--deep", "--strict", str(app)])
    else:
        dest_dir = Path.home() / "Applications"
        dest_dir.mkdir(exist_ok=True)
        dest = dest_dir / "Soul-Player.AppImage"
        # Write beside the target and rename into place, rather than copying
        # over it. Overwriting a file the kernel still holds as a running
        # executable fails with ETXTBSY ("Text file busy"), and a previous
        # scenario's app can still be exiting, the install-on-quit case
        # especially, where the installer it spawns outlives the quit. A
        # rename swaps the directory entry instead of the bytes, so a lingering
        # process keeps its own inode and the new build lands atomically.
        #
        # This is what broke the aarch64 release e2e at the upgrade step, after
        # six scenarios had passed.
        staging = dest.with_name(dest.name + ".new")
        shutil.copy2(installer, staging)
        staging.chmod(0o755)
        os.replace(staging, dest)
        inst = Install()
        inst.exe = dest
        m = re.search(r"_(\d+\.\d+\.\d+)_", installer.name)
        inst.version = m.group(1) if m else ""
    check(inst.exe.is_file(), f"installed executable {inst.exe} missing")
    ok(f"installed {inst.version} at {inst.exe}")
    return inst


def uninstall(inst: Install, strict: bool = True) -> None:
    log("  uninstalling")
    if IS_WIN:
        exe = inst.entry["uninstall"].strip().strip('"')
        exe = exe.split('"')[0] if '"' in exe else exe
        # NSIS copies its uninstaller to %TEMP% and returns at once, so poll for the result.
        run([exe, "/S"], timeout=300)
        gone = wait_until(lambda: not WindowsInstall.entries() and not inst.exe.exists(), 180)
        check(gone, f"uninstall left {WindowsInstall.entries()} / {inst.exe}")
        if inst.dir.exists() and any(inst.dir.iterdir()):
            left = f"uninstall left files in {inst.dir}: {list(inst.dir.iterdir())}"
            if strict:
                raise E2EFailure(left)
            log(f"::warning::{left} (previous release; fixed since)")
            shutil.rmtree(inst.dir, ignore_errors=True)
    elif IS_MAC:
        shutil.rmtree(inst.app)
        check(not inst.app.exists(), "app bundle still present")
    else:
        inst.exe.unlink()
        check(not inst.exe.exists(), "AppImage still present")
    ok("uninstalled; nothing left in the install location")


# ---------------------------------------------------------------- exercising


def snap(inst: Install, env: AppEnv, out: Path, name: str, scene: str | None, library: Path) -> None:
    png = out / f"{name}.png"
    args = [str(inst.exe), "--snap", str(png), "--size", "1000x640"]
    if scene:
        args += ["--scene", scene]
    e = dict(env.env, SOUL_PLAYER_FOLDER=str(library))
    r = subprocess.run(args, env=e, timeout=180, capture_output=True, text=True)
    check(r.returncode == 0, f"--snap {name} exited {r.returncode}\n{r.stdout}\n{r.stderr}")
    ok(f"rendered {name}: {png_check(png, 1000, 640)}")


def live_run(
    inst: Install, env: AppEnv, version: str, library: Path, secs: int = 20, require_log: bool = True
) -> str:
    """Run the real windowed app, autoplaying the library, then quit it. Returns its log.

    `require_log=False` for previous releases that may predate file logging (< 0.4):
    their log is checked only if they wrote one."""
    mark = env.log_mark()
    e = dict(env.env, SOUL_PLAYER_FOLDER=str(library), SOUL_PLAYER_AUTOPLAY="1")
    console = env.home / f"console-{time.monotonic_ns()}.txt"
    with open(console, "wb") as out:
        p = subprocess.Popen([str(inst.exe)], env=e, stdout=out, stderr=subprocess.STDOUT)
    try:
        time.sleep(secs)
        if p.poll() is not None:
            detail = console.read_text(errors="replace")[-4000:]
            if sys.platform.startswith("linux") and str(inst.exe).endswith(".AppImage"):
                # Tell a FUSE/runtime problem from an app problem: run it unmounted.
                r = subprocess.run(
                    [str(inst.exe)], env=dict(e, APPIMAGE_EXTRACT_AND_RUN="1"),
                    capture_output=True, text=True, timeout=60,
                )
                detail += f"\n--- APPIMAGE_EXTRACT_AND_RUN=1: exit {r.returncode} ---\n{r.stdout[-2000:]}{r.stderr[-2000:]}"
            raise E2EFailure(
                f"app exited early with {p.returncode}:\n{detail}\n--- log ---\n{env.log_since(mark)[-4000:]}"
            )
        ok(f"stayed up for {secs}s")
    finally:
        if p.poll() is None:
            if IS_WIN:
                p.terminate()
            else:
                p.send_signal(signal.SIGTERM)  # a normal quit (see signal_quit.rs)
        try:
            p.wait(timeout=30)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
            raise E2EFailure("app did not quit within 30s of SIGTERM") from None
    if not IS_WIN:
        # Releases before 0.4 have no SIGTERM handler and die by the signal (-15).
        allowed = (0,) if require_log else (0, -signal.SIGTERM)
        check(p.returncode in allowed, f"app quit with status {p.returncode} after SIGTERM")
        ok(f"quit on SIGTERM (status {p.returncode})")
    text = env.log_since(mark)
    if not require_log and "Soul Player starting" not in text:
        log(f"  note: {version} writes no log file; checked that it runs only")
        return text
    check("Soul Player starting" in text, f"no startup line in the log ({env.log_dir})")
    m = re.search(r'Soul Player starting.*?version="?([0-9][^\s",]*)', text) or re.search(
        r'version="?([0-9][^\s",]*)"?.*Soul Player starting', text
    )
    check(m is not None and m.group(1) == version, f"log reports version {m and m.group(1)}, expected {version}")
    check("panicked" not in text, f"panic in the log:\n{text[-4000:]}")
    ok(f"log: version {version}, no panic")
    return text


def exercise(inst: Install, env: AppEnv, version: str, library: Path, out: Path, full: bool) -> None:
    out.mkdir(parents=True, exist_ok=True)
    scenes = [("library", None), ("settings-audio", "settings-audio"), ("albums", "demo-artists")]
    for name, scene in scenes if full else scenes[:1]:
        snap(inst, env, out, name, scene, library)
    text = live_run(inst, env, version, library)
    check("playing the loaded folder" in text, "autoplay did not start playback")
    ok("playback started")
    if not IS_WIN:
        # The SIGTERM quit must have counted as clean: a second launch reports no crash.
        text = live_run(inst, env, version, library, secs=8)
        check("did not exit cleanly" not in text, "the previous (SIGTERM) quit was reported as a crash")
        ok("relaunch sees a clean previous exit")


# ---------------------------------------------------------------- update check


class _Quiet(http.server.SimpleHTTPRequestHandler):
    requests: list[str] = []

    def log_message(self, *a):
        pass

    def do_GET(self):  # noqa: N802 (stdlib name)
        _Quiet.requests.append(self.path)
        super().do_GET()


def serve_dir(d: Path) -> tuple[http.server.ThreadingHTTPServer, str]:
    handler = functools.partial(_Quiet, directory=str(d))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_address[1]}"


def check_update_detected(inst: Install, env: AppEnv, manifest: Path, new_version: str, library: Path) -> None:
    d = Path(tempfile.mkdtemp())
    shutil.copy2(manifest, d / "latest.json")
    srv, base = serve_dir(d)
    try:
        mark = env.log_mark()
        _Quiet.requests.clear()
        e = dict(env.env, SOUL_PLAYER_FOLDER=str(library), SOUL_PLAYER_UPDATE_ENDPOINT=f"{base}/latest.json")
        e.pop("SOUL_PLAYER_NO_UPDATE", None)
        p = subprocess.Popen([str(inst.exe)], env=e, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        # Proof the check ran and accepted the manifest: the app logs "update available
        # <version>" (0.4+). Releases that predate file logging can only show that they
        # fetched the manifest from us.
        pat = re.compile(r"update available.*" + re.escape(new_version))

        def detected() -> bool:
            if pat.search(env.log_since(mark)):
                return True
            if "/latest.json" not in _Quiet.requests:
                return False
            time.sleep(5)  # give a logging build time to write its verdict
            return not env.log_since(mark)  # a log-less build: the fetch is all we can see

        try:
            found = wait_until(detected, 60)
        finally:
            p.terminate() if IS_WIN else p.send_signal(signal.SIGTERM)
            try:
                p.wait(timeout=30)
            except subprocess.TimeoutExpired:
                p.kill()
        text = env.log_since(mark)
        check(found, f"previous release did not detect {new_version} (requests {_Quiet.requests}):\n{text[-3000:]}")
        if pat.search(text):
            ok(f"previous release's updater offers {new_version} from the new latest.json")
        else:
            ok("previous release fetched the new latest.json (it writes no log to confirm the offer)")
    finally:
        srv.shutdown()


# ---------------------------------------------------------------- auto-update

STALE = b"\0soul-e2e-stale-build\0"


def installed_version(inst: Install) -> str:
    """What the OS now records for the install (re-read, not cached)."""
    if IS_WIN:
        entries = WindowsInstall.entries()
        check(len(entries) == 1, f"expected exactly one '{APP_NAME}' install, found {entries}")
        return entries[0]["version"]
    if IS_MAC:
        return subprocess.check_output(
            ["/usr/libexec/PlistBuddy", "-c", "Print :CFBundleShortVersionString",
             str(inst.app / "Contents" / "Info.plist")], text=True,
        ).strip()
    return ""  # an AppImage records nothing outside itself


def mark_stale(inst: Install) -> None:
    """Tag the installed files so their replacement is observable even when the update
    carries byte-identical contents: a trailing overlay on the executable (ignored by
    PE and AppImage loaders), or a file inside the bundle on macOS (appending to a
    Mach-O would break its signature)."""
    if IS_MAC:
        (inst.app / "Contents" / "Resources" / "soul-e2e-stale").write_bytes(STALE)
    elif IS_WIN:
        # The previous run's exe can stay locked for a moment after it exits.
        deadline = time.monotonic() + 30
        while True:
            try:
                with open(inst.exe, "ab") as f:
                    f.write(STALE)
                return
            except PermissionError:
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.5)
    else:
        # Write a marked copy and swap it in: appending in place fails with ETXTBSY
        # while the previous run's AppImage runtime (its FUSE mount) is still
        # winding down, whereas a rename just gives the path a new inode.
        tmp = inst.exe.with_name(inst.exe.name + ".e2e-stale")
        shutil.copy2(inst.exe, tmp)
        with open(tmp, "ab") as f:
            f.write(STALE)
        os.replace(tmp, inst.exe)


def is_stale(inst: Install) -> bool:
    if IS_MAC:
        return (inst.app / "Contents" / "Resources" / "soul-e2e-stale").exists()
    try:
        with open(inst.exe, "rb") as f:
            f.seek(-len(STALE), os.SEEK_END)
            return f.read() == STALE
    except OSError:
        return True  # mid-replacement


def pid_alive(pid: int) -> bool:
    if IS_WIN:
        r = subprocess.run(["tasklist", "/FI", f"PID eq {pid}", "/NH", "/FO", "CSV"],
                           capture_output=True, text=True)
        return f'"{pid}"' in r.stdout
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def stop_pid(pid: int) -> None:
    """Quit a process we did not start (the updater relaunched it) and wait for it."""
    if IS_WIN:
        subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True)
    else:
        os.kill(pid, signal.SIGTERM)
    if not wait_until(lambda: not pid_alive(pid), 30, 0.5):
        if not IS_WIN:
            os.kill(pid, signal.SIGKILL)
        raise E2EFailure(f"relaunched app (pid {pid}) did not quit within 30s")


class UpdateServer:
    """Serves a manifest offering `version` → `artifact` (with its real signature) until
    the artifact has been downloaded, then answers 404, so the relaunched build, which
    still has the endpoint in its environment, sees no update and stays up instead of
    updating in a loop."""

    def __init__(self, version: str, target_key: str, artifact: Path, signature: str, fmt: str,
                 rate: int | None = None, drop_first_at: float | None = None):
        """`rate` (bytes/s) throttles the download, like a slow connection.
        `drop_first_at` (0..1) cuts the FIRST download off after that fraction,
        like a dropped connection; later ones complete."""
        self.requests: list[str] = []
        self.offering = True  # False: "no release yet" (404), until flipped
        self.delivered = False  # a complete artifact went out
        self.size = artifact.stat().st_size
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_GET(self):  # noqa: N802 (stdlib name)
                outer.requests.append(self.path)
                if (self.path.startswith("/latest.json") and outer.offering
                        and not outer.delivered):
                    body, ctype = json.dumps(outer.manifest).encode(), "application/json"
                elif self.path == "/artifact":
                    body, ctype = artifact.read_bytes(), "application/octet-stream"
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                if self.path != "/artifact":
                    self.wfile.write(body)
                    return
                first = outer.requests.count("/artifact") == 1
                cut = (int(len(body) * drop_first_at) if first and drop_first_at is not None
                       else len(body))
                chunk, sent, t0 = 64 * 1024, 0, time.monotonic()
                try:
                    while sent < cut:
                        n = min(chunk, cut - sent)
                        self.wfile.write(body[sent:sent + n])
                        sent += n
                        if rate:  # hold the average to `rate`
                            ahead = sent / rate - (time.monotonic() - t0)
                            if ahead > 0:
                                time.sleep(ahead)
                except (BrokenPipeError, ConnectionResetError):
                    return
                if sent < len(body):
                    # Hang up mid-body: the client sees the connection drop.
                    self.close_connection = True
                    self.connection.shutdown(2)
                    return
                outer.delivered = True

        self.srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        base = f"http://127.0.0.1:{self.srv.server_address[1]}"
        self.endpoint = f"{base}/latest.json"
        self.manifest = {
            "version": version,
            "pub_date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "notes": "release e2e",
            "platforms": {target_key: {"url": f"{base}/artifact", "signature": signature, "format": fmt}},
        }
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()

    def manifest_requests(self) -> int:
        return sum(1 for r in self.requests if r.startswith("/latest.json"))

    def close(self) -> None:
        self.srv.shutdown()


STARTING = re.compile(r"Soul Player starting(?P<rest>.*)")


def startups(text: str) -> list[dict[str, str]]:
    """Every startup line in a log chunk, with its `version` and `pid` fields."""
    out = []
    for m in STARTING.finditer(text):
        line = text[text.rfind("\n", 0, m.start()) + 1 : m.end()]
        fields = dict(re.findall(r'(\w+)="?([^\s"]+)"?', line))
        out.append(fields)
    return out


def auto_update(
    inst: Install, env: AppEnv, library: Path, offer: UpdateServer, expect_version: str
) -> str:
    """Let the installed app update itself through its own updater, unattended, and check
    the outcome. Returns the log of the whole episode."""
    mark_stale(inst)
    check(is_stale(inst), "could not tag the installed build")
    mark = env.log_mark()
    e = dict(env.env, SOUL_PLAYER_FOLDER=str(library), SOUL_PLAYER_AUTOPLAY="1",
             SOUL_PLAYER_UPDATE_ENDPOINT=offer.endpoint, SOUL_PLAYER_AUTO_INSTALL_UPDATE="1")
    e.pop("SOUL_PLAYER_NO_UPDATE", None)
    console = env.home / f"console-update-{time.monotonic_ns()}.txt"
    with open(console, "wb") as out:
        p = subprocess.Popen([str(inst.exe)], env=e, stdout=out, stderr=subprocess.STDOUT)

    def detail() -> str:
        return (f"requests {offer.requests}\n--- console ---\n{console.read_text(errors='replace')[-3000:]}"
                f"\n--- log ---\n{env.log_since(mark)[-5000:]}")

    # 1. The running build finds the update, downloads + verifies it, and quits on its own.
    failed = re.compile(r"update (download|install) failed|restarting failed|path is unknown")
    quit_or_failed = wait_until(lambda: p.poll() is not None or failed.search(env.log_since(mark)), 180)
    if p.poll() is None:
        p.kill()
        p.wait()
        why = "the updater reported an error" if quit_or_failed else "it did not quit within 180s"
        raise E2EFailure(f"auto-update failed: {why}\n{detail()}")
    check(p.returncode == 0, f"the app exited {p.returncode} while updating\n{detail()}")
    check("/artifact" in offer.requests, f"the update was never downloaded\n{detail()}")
    ok(f"downloaded + verified the signed update offered as {offer.manifest['version']}, quit cleanly")

    # 2. The OS-level install replaces the files and the new build comes back up.
    def replaced() -> bool:
        """Whether the install really put new files in place.

        `is_stale` reads `inst.exe` and reports True when it cannot, to cover
        the mid-replacement window, but an update that RENAMES the binary
        deletes that path for good, so a missing file would read as "still
        stale" forever. Windows does relaunch correctly across a rename (its
        installer handles the restart, not `relaunch_target`), and without this
        it would be indistinguishable from a failure.
        """
        return not is_stale(inst) or renamed_binary() is not None

    def relaunched() -> dict[str, str] | None:
        later = [s for s in startups(env.log_since(mark)) if s.get("pid") != str(p.pid)]
        return later[-1] if later and replaced() else None

    def renamed_binary() -> Path | None:
        """The replacement binary, when this update RENAMED it.

        A rename deletes `inst.exe` as part of a completely successful install,
        and the relaunch is performed by the OLD build using its own
        `current_exe`: so it starts a path that no longer exists and logs
        "restarting failed: No such file or directory". That cannot be fixed for
        an upgrade FROM an already-published build: the code doing the relaunch
        already shipped. (`update.rs: relaunch_target` is fixed for future
        renames, which does nothing for this one.)

        Detected rather than declared, so there is no flag to remember to
        remove: it resolves to None whenever the binary name is unchanged, and
        a genuine relaunch failure in that (normal) case still fails below.
        """
        if inst.exe.exists():
            return None
        d = inst.exe.parent
        if not d.is_dir():
            return None
        cands = [
            f for f in sorted(d.iterdir())
            if f.is_file() and "unins" not in f.name.lower()
            and (f.suffix.lower() == ".exe" if IS_WIN else os.access(f, os.X_OK))
        ]
        return cands[0] if len(cands) == 1 else None

    if not wait_until(lambda: relaunched() is not None, 180):
        moved = renamed_binary()
        check(moved is not None, f"no relaunch on replaced files\n{detail()}")
        # The update itself worked; only the restart did not. Verify that much
        # by hand, then stop, the remaining assertions are about the relaunched
        # process, which by definition does not exist here.
        log(f"  note: the update renamed the binary to {moved.name}; the previous build "
            f"relaunched its own (now absent) path, so there is no auto-restart "
            f"across this one upgrade")
        mark2 = env.log_mark()
        q = subprocess.Popen([str(moved)], env=dict(env.env, SOUL_PLAYER_FOLDER=str(library)),
                             stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
        try:
            got = None
            if wait_until(lambda: bool(startups(env.log_since(mark2))), 120):
                got = startups(env.log_since(mark2))[-1]
            check(got is not None,
                  f"the renamed binary {moved.name} does not start\n{detail()}")
            check(got.get("version") == expect_version,
                  f"{moved.name} reports {got.get('version')}, expected {expect_version}")
            ok(f"the update installed {expect_version} as {moved.name}; it runs when started")
        finally:
            q.terminate()
            try:
                q.wait(timeout=30)
            except subprocess.TimeoutExpired:
                q.kill()
        return env.log_since(mark)

    new = relaunched()
    pid = int(new["pid"])
    ok(f"installed files replaced; relaunched as pid {pid}")
    try:
        check(new.get("version") == expect_version,
              f"relaunched build reports {new.get('version')}, expected {expect_version}")
        reported = installed_version(inst)
        check(reported in ("", expect_version), f"the OS now records {reported}, expected {expect_version}")
        time.sleep(10)
        check(pid_alive(pid), f"relaunched app exited early\n{detail()}")
        text = env.log_since(mark)
        after = text[text.rfind("Soul Player starting"):]
        check("did not exit cleanly" not in after, "the update's quit was reported as a crash")
        check("panicked" not in text, f"panic during the update:\n{text[-4000:]}")
        ok(f"running {expect_version} after the update; no crash reported, no panic")
    finally:
        if pid_alive(pid):
            stop_pid(pid)
    return env.log_since(mark)


# ---------------------------------------------------------------- update settings


def config_file(env: AppEnv) -> Path:
    """The app's settings file (`config.rs`: `<config dir>/soul-player/config.toml`)."""
    if IS_WIN:
        return Path(os.environ["APPDATA"]) / "soul-player" / "config.toml"
    if IS_MAC:
        return env.home / "Library" / "Application Support" / "soul-player" / "config.toml"
    return env.home / ".config" / "soul-player" / "config.toml"


def set_config(env: AppEnv, **values: bool) -> None:
    """Set top-level boolean keys in the app's config.toml the way the Settings
    switches persist them (the app keeps every other key it wrote)."""
    f = config_file(env)
    f.parent.mkdir(parents=True, exist_ok=True)
    lines = f.read_text(encoding="utf-8").splitlines() if f.exists() else []
    lines = [ln for ln in lines if not any(re.match(rf"\s*{k}\s*=", ln) for k in values)]
    head = [f"{k} = {'true' if v else 'false'}" for k, v in values.items()]
    f.write_text("\n".join(head + lines) + "\n", encoding="utf-8")  # top-level keys first


def graceful_quit(p: subprocess.Popen, timeout: int = 60, apple_event: bool = False) -> int:
    """Quit the way a user does: SIGTERM (handled like ⌘Q) or, on Windows, a
    window close (WM_CLOSE via taskkill without /F). Returns the exit status.

    `apple_event` (macOS): send the Quit Apple Event instead, the path ⌘Q, the
    Dock and logout take. AppKit then ends the process from
    `applicationWillTerminate:` and the app's run loop never returns, unlike the
    SIGTERM quit, which the app turns into its own quit. 0.4.1 installed an
    install-on-quit update only after the run loop returned, so a real ⌘Q never
    installed it while every SIGTERM-driven check passed."""
    if IS_WIN:
        subprocess.run(["taskkill", "/PID", str(p.pid)], capture_output=True)
    elif IS_MAC and apple_event:
        r = subprocess.run(["osascript", "-e", f'tell application id "{BUNDLE_ID}" to quit'],
                           capture_output=True, text=True, timeout=60)
        check(r.returncode == 0, f"the Quit Apple Event failed: {r.stderr.strip()}")
    else:
        p.send_signal(signal.SIGTERM)
    try:
        return p.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait()
        raise E2EFailure(f"app did not quit within {timeout}s") from None


def launch_with_updater(inst: Install, env: AppEnv, library: Path, offer: UpdateServer,
                        interval: int | None = None) -> tuple[subprocess.Popen, dict, Path]:
    e = dict(env.env, SOUL_PLAYER_FOLDER=str(library), SOUL_PLAYER_AUTOPLAY="1",
             SOUL_PLAYER_UPDATE_ENDPOINT=offer.endpoint)
    e.pop("SOUL_PLAYER_NO_UPDATE", None)
    if interval:
        e["SOUL_PLAYER_UPDATE_INTERVAL_SECS"] = str(interval)
    mark = env.log_mark()
    console = env.home / f"console-sched-{time.monotonic_ns()}.txt"
    with open(console, "wb") as out:
        p = subprocess.Popen([str(inst.exe)], env=e, stdout=out, stderr=subprocess.STDOUT)
    return p, mark, console


def scheduled_check(inst: Install, env: AppEnv, library: Path, update: tuple, version: str, key: str) -> None:
    """A release published while the app runs is found by a scheduled re-check, and,
    with the default settings, offered but never installed on its own."""
    set_config(env, update_auto_check=True, update_auto_install=False)
    offered = bump_patch(version)
    offer = UpdateServer(offered, key, *update)
    offer.offering = False  # nothing released yet
    mark_stale(inst)
    p, mark, _ = launch_with_updater(inst, env, library, offer, interval=4)
    try:
        check(wait_until(lambda: offer.manifest_requests() >= 1, 60), "no launch-time check")
        ok("launch check ran (no release yet)")
        time.sleep(3)
        offer.offering = True  # the release goes live while the app is running
        pat = re.compile(r"update available.*" + re.escape(offered))
        check(wait_until(lambda: pat.search(env.log_since(mark)) is not None, 60),
              f"a running app did not pick up the new release\n{env.log_since(mark)[-3000:]}")
        n = offer.manifest_requests()
        check(n >= 2, f"found on a re-check, expected ≥ 2 manifest requests, saw {n}")
        check(p.poll() is None, "the app restarted or exited by itself")
        ok(f"scheduled re-check #{n} found {offered} while the app kept running")
        time.sleep(6)
    finally:
        status = graceful_quit(p)
        offer.close()
    text = env.log_since(mark)
    check(status == 0, f"quit with {status}")
    check("/artifact" not in offer.requests, "it downloaded the update without being asked")
    check(is_stale(inst) and "installing update" not in text, "it installed the update on its own")
    ok("offered, not downloaded or installed (installing is the user's call)")


def auto_check_off(inst: Install, env: AppEnv, library: Path, update: tuple, version: str, key: str) -> None:
    """With "Check for updates automatically" off, the app never contacts the server."""
    set_config(env, update_auto_check=False, update_auto_install=False)
    offer = UpdateServer(bump_patch(version), key, *update)
    p, mark, _ = launch_with_updater(inst, env, library, offer, interval=2)
    try:
        time.sleep(15)
        check(p.poll() is None, "app exited early")
    finally:
        status = graceful_quit(p)
        offer.close()
        set_config(env, update_auto_check=True)
    check(status == 0, f"quit with {status}")
    check(not offer.requests, f"automatic checks are off, yet it requested {offer.requests}")
    check("update available" not in env.log_since(mark), "it found an update with checks off")
    ok("no update traffic in 15s with automatic checks off (re-checks every 2s otherwise)")


PROGRESS = re.compile(r"^(\S+)\s.*update download progress percent=(\d+)", re.M)


def check_progress(text: str, min_secs: float) -> None:
    """The log's 10 % download steps: in order, up to 100 %, spread over at
    least `min_secs` (the throttled server's pace, progress was reported as
    the bytes arrived, not all at the end)."""
    steps = [(m.group(1), int(m.group(2))) for m in PROGRESS.finditer(text)]
    pcts = [p for _, p in steps]
    check(len(pcts) >= 5 and pcts == sorted(set(pcts)) and pcts[-1] == 100,
          f"download progress steps {pcts}")

    def ts(s: str) -> float:
        return time.mktime(time.strptime(s[:19], "%Y-%m-%dT%H:%M:%S")) + float("0." + s[20:-1] or 0)

    span = ts(steps[-1][0]) - ts(steps[0][0])
    check(span >= min_secs, f"progress {pcts} came in {span:.1f}s, expected it spread over ≥{min_secs}s")
    ok(f"download progress reported as it arrived: {pcts} over {span:.1f}s")


def slow_rate(update: tuple, secs: float) -> int:
    """Bytes/s that makes this update's download take about `secs`."""
    return max(64 * 1024, int(update[0].stat().st_size / secs))


def install_on_quit(inst: Install, env: AppEnv, library: Path, update: tuple, version: str, key: str) -> None:
    """"Install updates automatically": download in the background, install nothing
    mid-session, install when the user quits (on macOS with the Quit Apple Event, as
    ⌘Q does), and do not reopen the app."""
    set_config(env, update_auto_check=True, update_auto_install=True)
    # A slow connection: the download takes ~8 s, so progress has to arrive
    # in steps while the app keeps running.
    offer = UpdateServer(bump_patch(version), key, *update, rate=slow_rate(update, 8))
    mark_stale(inst)
    p, mark, console = launch_with_updater(inst, env, library, offer)
    try:
        ready = wait_until(lambda: "installs when you quit" in env.log_since(mark)
                           or p.poll() is not None, 180)
        check(ready and p.poll() is None, f"no background download\n{env.log_since(mark)[-3000:]}")
        check("/artifact" in offer.requests, "staged without downloading?")
        check_progress(env.log_since(mark), 5)
        time.sleep(5)
        check(p.poll() is None, "the app quit/restarted on its own after downloading")
        check(is_stale(inst), "it installed mid-session")
        ok("downloaded + verified in the background; nothing installed while running")
    except BaseException:
        p.kill()
        offer.close()
        raise
    status = graceful_quit(p, apple_event=True)
    offer.close()
    check(status == 0, f"quit with {status}")
    check(wait_until(lambda: not is_stale(inst), 180),
          f"quitting did not install the update\n{env.log_since(mark)[-3000:]}")
    time.sleep(15)  # an unwanted relaunch would have logged a startup by now
    later = [x for x in startups(env.log_since(mark)) if x.get("pid") != str(p.pid)]
    check(not later, f"the app reopened after installing on quit: {later}")
    reported = installed_version(inst)
    check(reported in ("", version), f"the OS now records {reported}")
    ok("installed on quit, app stayed closed")
    set_config(env, update_auto_install=False)
    text = live_run(inst, env, version, library, secs=8)
    check("did not exit cleanly" not in text, "the quit that installed the update was reported as a crash")
    ok("next launch runs the installed build, no crash reported")


def move_from_dmg(installer: Path, work: Path, library: Path, version: str) -> None:
    """macOS: opened straight from its disk image, the app moves itself into
    Applications (unattended here; a prompt for users), relaunches from there,
    and ejects the image. The moved copy keeps its signature and loses the
    quarantine flag, so Gatekeeper doesn't translocate it on the next launch."""
    env = AppEnv(work, "move")
    apps = work / "moved-apps"
    apps.mkdir(exist_ok=True)
    mnt = Path(tempfile.mkdtemp(dir=work))
    attach_dmg(installer, mnt)
    moved = apps / f"{APP_NAME}.app"
    new_pid = None
    try:
        exe = next((mnt / f"{APP_NAME}.app" / "Contents" / "MacOS").iterdir())
        e = dict(env.env, SOUL_PLAYER_FOLDER=str(library), SOUL_PLAYER_MOVE_TO_APPLICATIONS="1",
                 SOUL_PLAYER_APPLICATIONS_DIR=str(apps))
        mark = env.log_mark()
        # Output to a file, not a pipe: the moved copy it starts inherits the
        # handles, and a pipe would stay open until that one exits too.
        with open(env.home / "console-move.txt", "wb") as out:
            r = subprocess.run([str(exe)], env=e, stdout=out, stderr=subprocess.STDOUT,
                               stdin=subprocess.DEVNULL, timeout=120)
        check(r.returncode == 0, f"the copy on the disk image exited with {r.returncode}")
        relaunched = re.compile(r"Soul Player starting.*pid=(\d+).*exe=" + re.escape(str(moved)))
        check(wait_until(lambda: relaunched.search(env.log_since(mark)) is not None, 60),
              f"the moved copy never started\n{env.log_since(mark)[-3000:]}")
        text = env.log_since(mark)
        new_pid = int(relaunched.search(text).group(1))
        check("place=DiskImage" in text, "it didn't recognise the disk image")
        ok(f"moved off the disk image to {moved}, relaunched as pid {new_pid}")
        run(["codesign", "--verify", "--deep", "--strict", str(moved)])
        attrs = subprocess.run(["xattr", str(moved)], capture_output=True, text=True).stdout
        check("com.apple.quarantine" not in attrs, "the moved copy is still quarantined")
        ok("moved copy: signature intact, not quarantined")
        check(wait_until(lambda: not os.path.ismount(mnt), 30), "the disk image was not ejected")
        ok("disk image ejected")
        time.sleep(5)
        check(pid_alive(new_pid), "the moved copy exited")
        n = len(startups(env.log_since(mark)))
        check(n == 2, f"expected the disk-image copy and one moved copy to start, saw {n} startups")
        check(installed_version_at(moved) == version, f"moved copy reports {installed_version_at(moved)}")
    finally:
        if new_pid and pid_alive(new_pid):
            stop_pid(new_pid)
        if os.path.ismount(mnt):
            eject_dmg(mnt)
        shutil.rmtree(apps, ignore_errors=True)


def installed_version_at(app: Path) -> str:
    return subprocess.check_output(
        ["/usr/libexec/PlistBuddy", "-c", "Print :CFBundleShortVersionString", str(app / "Contents" / "Info.plist")],
        text=True).strip()


def interrupted_download(inst: Install, env: AppEnv, library: Path, update: tuple, version: str,
                         key: str) -> None:
    """The connection drops 40 % into a background download: the app says so,
    keeps running, retries on its own soon after (not at the next 12-hour
    check), and the retry completes and verifies."""
    set_config(env, update_auto_check=True, update_auto_install=True)
    offer = UpdateServer(bump_patch(version), key, *update, rate=slow_rate(update, 4),
                         drop_first_at=0.4)
    mark_stale(inst)
    p, mark, _ = launch_with_updater(inst, env, library, offer, interval=3)
    try:
        failed = wait_until(lambda: "update download failed" in env.log_since(mark)
                            or p.poll() is not None, 120)
        check(failed and p.poll() is None,
              f"the dropped download wasn't reported\n{env.log_since(mark)[-3000:]}")
        ok("the dropped connection was reported; the app kept running")
        ready = wait_until(lambda: "installs when you quit" in env.log_since(mark)
                           or p.poll() is not None, 120)
        check(ready and p.poll() is None, f"no retry after the drop\n{env.log_since(mark)[-3000:]}")
        n = offer.requests.count("/artifact")
        check(n == 2, f"expected one dropped and one complete download, saw {n}")
        check(is_stale(inst), "it installed mid-session")
        ok("retried on its own, downloaded in full and verified")
    finally:
        status = graceful_quit(p, apple_event=True)
        offer.close()
        set_config(env, update_auto_install=False)
    check(status == 0, f"quit with {status}")
    check(wait_until(lambda: not is_stale(inst), 180), "the retried download didn't install on quit")
    ok("installed on quit")
    check("panicked" not in env.log_since(mark), "panic")


def mac_archs(inst: Install) -> str:
    return subprocess.check_output(["lipo", "-archs", str(inst.exe)], text=True).strip()


def universal_dmg(installer: Path, work: Path, library: Path, update: tuple, version: str, key: str) -> None:
    """The universal DMG (one download for every Mac): both architectures in
    the bundle, runs natively here, and the in-app updater takes it to this
    Mac's own build."""
    env = AppEnv(work, "universal")
    inst = install(installer, work)
    try:
        archs = set(mac_archs(inst).split())
        check(archs == {"arm64", "x86_64"}, f"universal binary has {archs}")
        ok("universal binary: arm64 + x86_64")
        mark = env.log_mark()
        live_run(inst, env, version, library, secs=8)
        first = startups(env.log_since(mark))[0]
        native = "aarch64" if key == "macos-aarch64" else "x86_64"
        check(first.get("arch") == native and first.get("rosetta") == "false",
              f"ran as {first.get('arch')} (rosetta={first.get('rosetta')}), expected native {native}")
        ok(f"runs natively as {native}")
        offer = UpdateServer(bump_patch(version), key, *update)
        try:
            auto_update(inst, env, library, offer, version)
        finally:
            offer.close()
        thin = mac_archs(inst)
        check(thin == ("arm64" if native == "aarch64" else "x86_64"), f"after updating: {thin}")
        ok(f"updated in-app to this Mac's own build ({thin})")
    finally:
        uninstall(inst)


def rosetta_migration(installer: Path, work: Path, library: Path, update: tuple, version: str) -> None:
    """Apple silicon running the INTEL build (the wrong DMG): it knows it runs
    under Rosetta, and the in-app switch fetches the Apple silicon build of the
    SAME version, swaps it in and comes back native."""
    env = AppEnv(work, "rosetta")
    inst = install(installer, work)
    try:
        check(mac_archs(inst) == "x86_64", f"expected the Intel build, got {mac_archs(inst)}")
        mark = env.log_mark()
        live_run(inst, env, version, library, secs=8)
        first = startups(env.log_since(mark))[0]
        check(first.get("arch") == "x86_64" and first.get("rosetta") == "true",
              f"Intel build on Apple silicon not detected as Rosetta: {first}")
        ok("the Intel build knows it runs under Rosetta")
        # Same version, Apple silicon package: the switch doesn't wait for a release.
        offer = UpdateServer(version, "macos-aarch64", *update)
        try:
            text = auto_update(inst, env, library, offer, version)
        finally:
            offer.close()
        last = startups(text)[-1]
        check(last.get("arch") == "aarch64" and last.get("rosetta") == "false",
              f"after switching it runs as {last.get('arch')} (rosetta={last.get('rosetta')})")
        check(mac_archs(inst) == "arm64", f"installed binary is {mac_archs(inst)}")
        ok(f"switched to the Apple silicon build of {version}; runs natively")
    finally:
        uninstall(inst)


def data_dirs(env: AppEnv) -> list[Path]:
    """Every folder Soul Player writes for this profile (`uninstall::data_paths`)."""
    if IS_MAC:
        lib = env.home / "Library"
        return [lib / "Application Support" / "soul-player", lib / "Logs" / APP_NAME,
                lib / "Caches" / "soul-player"]
    if IS_WIN:
        return [Path(os.environ["APPDATA"]) / "soul-player", Path(os.environ["LOCALAPPDATA"]) / "soul-player"]
    return [env.home / ".config" / "soul-player", env.home / ".local" / "state" / "soul-player",
            env.home / ".cache" / "soul-player"]


def full_uninstall(installer: Path, work: Path, library: Path, version: str) -> None:
    """Settings → About → Uninstall, through the same code as `--uninstall`:
    after a real session has written settings, logs and caches, the app and
    every one of those is gone, the OS no longer lists it, and the music
    library is untouched."""
    env = AppEnv(work, "uninstall")
    inst = install(installer, work)
    live_run(inst, env, version, library, secs=8)
    set_config(env, update_auto_check=False)  # a settings file, as a user's would be
    before = [d for d in data_dirs(env) if d.exists()]
    check(config_file(env).parent in before and env.log_dir.exists(),
          f"the session wrote no settings/logs to remove: {before}")
    songs = sorted(p.name for p in library.iterdir())
    r = subprocess.run([str(inst.exe), "--uninstall"], env=env.env, capture_output=True, text=True,
                       timeout=120)
    log("  " + "\n  ".join((r.stdout + r.stderr).strip().splitlines()))
    check(r.returncode == 0, f"--uninstall exited {r.returncode}")
    left = [d for d in data_dirs(env) if d.exists()]
    check(not left, f"left behind: {left}")
    ok(f"removed settings, logs and caches ({len(before)} folders)")
    if IS_MAC:
        check(not inst.app.exists(), f"{inst.app} is still there")
        trashed = env.home / ".Trash" / f"{APP_NAME}.app"
        ok("app moved to the Trash" if trashed.exists() else "app removed")
    elif IS_WIN:
        gone = wait_until(lambda: not WindowsInstall.entries() and not inst.exe.exists(), 120)
        check(gone, f"still installed: {WindowsInstall.entries()} / {inst.exe.exists()}")
        ok("the uninstaller ran: no Apps & features entry, program files gone")
    else:
        check(not inst.exe.exists(), f"{inst.exe} is still there")
        ok("AppImage removed")
    check(sorted(p.name for p in library.iterdir()) == songs, "the music library changed")
    ok("music library untouched")


def homebrew_cask(dmg: Path, work: Path, library: Path, version: str) -> None:
    """`brew install --cask soul-player` and `brew uninstall --zap`, with the
    cask the release publishes (`gen-cask.py`) pointed at the DMG under test.
    Brew installs it to /Applications quarantined, the app runs, and zap leaves
    nothing: the cask's zap list matches what the app actually writes."""
    env = AppEnv(work, "brew")
    # Brew runs with the app's (throwaway) HOME so zap's `~` is where the app wrote.
    benv = dict(env.env, HOMEBREW_NO_AUTO_UPDATE="1", HOMEBREW_NO_INSTALL_FROM_API="1",
                HOMEBREW_NO_ANALYTICS="1", HOMEBREW_NO_ENV_HINTS="1")
    gen = Path(__file__).with_name("gen-cask.py")

    def brew(*args: str, timeout: int = 600) -> str:
        r = subprocess.run(["brew", *args], env=benv, capture_output=True, text=True, timeout=timeout)
        check(r.returncode == 0, f"brew {' '.join(args)} failed:\n{r.stdout[-2000:]}\n{r.stderr[-2000:]}")
        return r.stdout

    tap = "soulaudio/e2e"
    brew("tap-new", "--no-git", tap)
    # Newer Homebrew refuses casks from a third-party tap until it is trusted,
    # as a user of soulaudio/tap does once. Older Homebrew has no `trust`.
    has_trust = subprocess.run(["brew", "trust", "--help"], env=benv, capture_output=True).returncode == 0
    if has_trust:
        brew("trust", tap)
    try:
        casks = Path(brew("--repository", tap).strip()) / "Casks"
        casks.mkdir(parents=True, exist_ok=True)
        # The cask as published (GitHub URL): lint it.
        run([sys.executable, str(gen), "--version", version, "--dmg", str(dmg),
             "--out", str(casks / "soul-player.rb")])
        brew("style", "--cask", f"{tap}/soul-player")
        brew("audit", "--cask", "--strict", f"{tap}/soul-player")
        ok("published cask passes brew style + audit")
        # The same cask pointed at the DMG under test.
        url = "file://" + urllib.request.pathname2url(str(dmg.resolve()))
        run([sys.executable, str(gen), "--version", version, "--dmg", str(dmg), "--url", url,
             "--out", str(casks / "soul-player.rb")])
        brew("install", "--cask", f"{tap}/soul-player")
        app = Path("/Applications") / f"{APP_NAME}.app"
        check(app.is_dir(), "brew didn't install /Applications/Soul Player.app")
        inst = Install()
        inst.app = app
        inst.exe = app / "Contents" / "MacOS" / subprocess.check_output(
            ["/usr/libexec/PlistBuddy", "-c", "Print :CFBundleExecutable", str(app / "Contents" / "Info.plist")],
            text=True).strip()
        inst.version = installed_version_at(app)
        check(inst.version == version, f"brew installed {inst.version}")
        attrs = subprocess.run(["xattr", str(app)], capture_output=True, text=True).stdout
        ok(f"brew installed {version} to /Applications"
           + (" (quarantined, as Gatekeeper expects)" if "quarantine" in attrs else ""))
        # What "Open Anyway" does for an app that isn't notarized yet.
        subprocess.run(["xattr", "-dr", "com.apple.quarantine", str(app)], check=False)
        live_run(inst, env, version, library, secs=8)
        wrote = [d for d in data_dirs(env) if d.exists()]
        check(wrote, "the session wrote nothing for zap to remove")
        brew("uninstall", "--cask", "--zap", f"{tap}/soul-player")
        check(not app.exists(), "brew uninstall left the app")
        left = [d for d in data_dirs(env) if d.exists()]
        check(not left, f"brew --zap left behind: {left}")
        ok(f"brew uninstall --zap removed the app and all {len(wrote)} data folders")
    finally:
        subprocess.run(["brew", "untap", tap], env=benv, capture_output=True)


def bump_patch(v: str) -> str:
    a, b, c = (semver(v) + (0, 0, 0))[:3]
    return f"{a}.{b}.{c + 1}"


def supports_auto_install(version: str) -> bool:
    """`SOUL_PLAYER_AUTO_INSTALL_UPDATE` and the pid on the startup line arrived after 0.3."""
    return semver(version) > (0, 3, 0)


# ---------------------------------------------------------------- previous release


def fetch_previous(url: str, target_key: str, work: Path) -> tuple[str, Path] | None:
    try:
        with urllib.request.urlopen(url, timeout=60) as r:
            man = json.load(r)
    except Exception as e:  # noqa: BLE001, no previous release is a normal state
        log(f"::notice::no previous release manifest at {url} ({e}); upgrade/rollback skipped")
        return None
    plat = man.get("platforms", {}).get(target_key)
    if not plat:
        log(f"::notice::previous release {man.get('version')} has no {target_key} build; upgrade/rollback skipped")
        return None
    name = plat["url"].rsplit("/", 1)[-1]
    dest = work / "previous" / name
    dest.parent.mkdir(parents=True, exist_ok=True)
    log(f"  downloading previous {man['version']}: {plat['url']}")
    urllib.request.urlretrieve(plat["url"], dest)
    return man["version"], dest


# ---------------------------------------------------------------- main


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--installer", required=True, type=Path)
    ap.add_argument("--version", required=True)
    ap.add_argument("--target-key", required=True, help="updater platform key, e.g. macos-aarch64")
    ap.add_argument("--manifest", type=Path, help="the new release's latest.json (for the update check)")
    ap.add_argument("--update-meta", type=Path, help="this build's <key>.meta.json updater sidecar")
    ap.add_argument(
        "--previous-manifest",
        help="URL of the currently published latest.json (default: none → upgrade/rollback skipped)",
    )
    ap.add_argument("--universal-installer", type=Path,
                    help="macOS: the universal DMG, installed and updated here too")
    ap.add_argument("--rosetta-installer", type=Path,
                    help="macOS on Apple silicon: the Intel DMG, to check the switch to Apple silicon")
    ap.add_argument("--out", type=Path, default=Path("e2e-out"))
    a = ap.parse_args()
    # Never hang silently until the job timeout: dump every thread's stack and abort.
    faulthandler.dump_traceback_later(20 * 60, exit=True)

    installer = a.installer.resolve()
    # Absolute: an AppImage's launcher runs the app from inside its own mount.
    a.out = a.out.resolve()
    check(installer.is_file(), f"installer {installer} not found")
    a.out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="soul-e2e-"))
    library = work / "music" / "Release E2E Album"
    write_fixture_library(library)
    steps: list[tuple[str, str]] = []

    try:
        # 1. fresh install
        section(f"fresh install of {a.version}")
        env = AppEnv(work, "fresh")
        inst = install(installer, work)
        check(inst.version in ("", a.version), f"installed version {inst.version}, expected {a.version}")
        exercise(inst, env, a.version, library, a.out / "fresh", full=True)
        steps.append(("fresh install → run", "pass"))
        end_section()

        if IS_MAC and installer.name.endswith(".dmg"):
            section("run from the disk image → moves itself to Applications")
            move_from_dmg(installer, work, library, a.version)
            steps.append(("opened from the DMG → moves to Applications, relaunches, ejects", "pass"))
            end_section()

        update = None
        if a.update_meta:
            meta = json.loads(a.update_meta.read_text(encoding="utf-8"))
            check(meta["target_key"] == a.target_key, f"sidecar is for {meta['target_key']}")
            artifact = a.update_meta.resolve().parent / meta["artifact"]
            check(artifact.is_file(), f"update artifact {artifact} missing")
            update = (artifact, meta["signature"].strip(), meta["format"])
            section(f"auto-update {a.version} → (offered) {bump_patch(a.version)}")
            offer = UpdateServer(bump_patch(a.version), a.target_key, *update)
            try:
                auto_update(inst, env, library, offer, a.version)
            finally:
                offer.close()
            steps.append(("auto-update via the app's updater (download → verify → install → relaunch)", "pass"))
            end_section()

            for title, fn in [
                ("scheduled re-check finds a new release; default settings never install", scheduled_check),
                ("automatic checks off → no update traffic", auto_check_off),
                ("install updates automatically → slow background download with progress, installs on quit",
                 install_on_quit),
                ("connection drops mid-download → reported, retried, installs on quit", interrupted_download),
            ]:
                section(f"update settings: {title}")
                fn(inst, env, library, update, a.version, a.target_key)
                steps.append((title, "pass"))
                end_section()
        else:
            steps.append(("auto-update", "skipped (no --update-meta)"))

        section("uninstall")
        uninstall(inst)
        steps.append(("uninstall, nothing left", "pass"))
        end_section()

        # Before the upgrade/rollback legs: an uninstall that leaves anything
        # behind fails the release here, before anything is published.
        section("full uninstall: the app and everything it stored")
        full_uninstall(installer, work, library, a.version)
        steps.append(("full uninstall (Settings → About): app, settings, logs, caches gone; music kept", "pass"))
        end_section()

        if a.universal_installer and update:
            section("universal DMG → runs natively → updates to this Mac's build")
            universal_dmg(a.universal_installer.resolve(), work, library, update, a.version, a.target_key)
            steps.append(("universal DMG: both architectures, native here, updates in-app", "pass"))
            end_section()
        if a.universal_installer and shutil.which("brew"):
            section("Homebrew: brew install --cask → runs → brew uninstall --zap")
            homebrew_cask(a.universal_installer.resolve(), work, library, a.version)
            steps.append(("Homebrew cask: style + audit, install, run, uninstall --zap leaves nothing", "pass"))
            end_section()
        if a.rosetta_installer and update:
            section("Intel build on Apple silicon → switches itself to the Apple silicon build")
            rosetta_migration(a.rosetta_installer.resolve(), work, library, update, a.version)
            steps.append(("Intel build under Rosetta → switches to Apple silicon, same version", "pass"))
            end_section()

        # 2 + 3. upgrade from, then roll back to, the previous release
        prev = fetch_previous(a.previous_manifest, a.target_key, work) if a.previous_manifest else None
        if prev is None:
            steps.append(("upgrade / rollback", "skipped (no previous build for this platform)"))
        else:
            prev_version, prev_installer = prev
            env = AppEnv(work, "upgrade")  # one profile: config carries across versions

            section(f"upgrade {prev_version} → {a.version}")
            old = install(prev_installer, work, current_build=False)
            # The previous release is history: if it can't run on today's runner that is
            # reported, not fatal, but installing over it, and getting back to it, are.
            try:
                live_run(old, env, prev_version, library, secs=10, require_log=False)
                prev_runs = True
            except E2EFailure as e:
                prev_runs = False
                log(f"::warning::previous release {prev_version} does not run on this runner: {e}")
                steps.append((f"previous {prev_version} runs here", "WARNING: no, see log"))
            older = semver(prev_version) < semver(a.version)
            if prev_runs and a.manifest and older:
                check_update_detected(old, env, a.manifest, a.version, library)
                steps.append((f"{prev_version} detects {a.version} as an update", "pass"))
            elif not older:
                log(f"::notice::{prev_version} is not older than {a.version}; update detection skipped")
            if prev_runs and older and update and supports_auto_install(prev_version):
                offer = UpdateServer(a.version, a.target_key, *update)
                try:
                    auto_update(old, env, library, offer, a.version)
                finally:
                    offer.close()
                steps.append((f"{prev_version} auto-updates itself to {a.version}", "pass"))
                new = old
                new.version = a.version
                # Re-read where the app actually lives now, rather than trusting
                # the pre-update object: an update may RENAME the binary, and
                # `old.exe` then points at a path the update deleted. Windows
                # already re-read its registry entry; macOS has to re-read the
                # bundle's own record of its executable, or everything after this
                # tries to launch the previous build's name and dies with
                # ENOENT. (That is what the rename did here: the upgrade
                # succeeded, then `exercise` failed on
                # .../MacOS/soul-player-rune.)
                if IS_WIN:  # re-read: the installer may have rewritten the registry entry
                    new = WindowsInstall(WindowsInstall.entries()[0])
                elif IS_MAC:
                    new.exe = new.app / "Contents" / "MacOS" / subprocess.check_output(
                        ["/usr/libexec/PlistBuddy", "-c", "Print :CFBundleExecutable",
                         str(new.app / "Contents" / "Info.plist")], text=True
                    ).strip()
            else:
                if older and update:
                    log(f"::notice::{prev_version} cannot install updates unattended; installing over it")
                new = install(installer, work)
            check(new.version in ("", a.version), f"after upgrade the OS reports {new.version}")
            exercise(new, env, a.version, library, a.out / "upgraded", full=False)
            steps.append((f"upgrade {prev_version} → {a.version}", "pass"))
            end_section()

            section(f"rollback {a.version} → {prev_version}")
            uninstall(new)
            old = install(prev_installer, work, current_build=False)
            check(old.version in ("", prev_version), f"after rollback the OS reports {old.version}")
            if prev_runs:  # it ran before the upgrade, so it must run on what the new one left
                live_run(old, env, prev_version, library, secs=12, require_log=False)
            uninstall(old, strict=False)
            steps.append((f"rollback {a.version} → {prev_version} → uninstall", "pass"))
            end_section()
    except (E2EFailure, subprocess.SubprocessError, OSError) as e:
        end_section()
        log(f"::error::release e2e failed: {e}")
        steps.append(("FAILED", str(e).splitlines()[0] if str(e) else type(e).__name__))
        write_summary(a, steps)
        return 1

    write_summary(a, steps)
    log("\nrelease e2e passed")
    return 0


def write_summary(a, steps) -> None:
    lines = [f"### Release e2e, {a.target_key} {a.version}", "", "| scenario | result |", "|---|---|"]
    lines += [f"| {s} | {r} |" for s, r in steps]
    text = "\n".join(lines) + "\n"
    log("\n" + text)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as f:
            f.write(text)


if __name__ == "__main__":
    raise SystemExit(main())
