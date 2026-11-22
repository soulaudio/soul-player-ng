#!/usr/bin/env python3
"""Release e2e on the runner's own OS: install → exercise → uninstall, then
upgrade-from-the-previous-release → rollback → uninstall.

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
  2. upgrade    install the PREVIOUS published release; check that it detects this release
                as an update (its updater reads the new `latest.json`, served locally);
                install the new build over it; check exactly one install remains at the
                new version and it runs with the old version's config.
  3. rollback   uninstall the new build, reinstall the previous one, and check it still
                runs against the config the new version left behind (the path a user takes
                back after a bad release); uninstall.

"Exercise" = headless renders of real views (GPU, fonts, layout) with a library scanned
from generated tagged tracks, then the real windowed app autoplaying that library for a
while: it must stay up, report the expected version in its log, start playback, never
panic, and (Unix) quit cleanly on SIGTERM so the next launch sees no crash.

Usage:
  release-e2e.py --installer PATH --version X.Y.Z --target-key KEY \
                 [--manifest new-latest.json] [--previous-manifest URL] [--out DIR]
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
BUNDLE_ID = "co.datadir.soulplayer"


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
    own profile is used — the runner is a throwaway VM anyway.
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


def install(installer: Path, work: Path, verify_sig: bool = True) -> Install:
    log(f"  installing {installer.name}")
    if IS_WIN:
        run([str(installer), "/S"], timeout=600)
        entries = WindowsInstall.entries()
        check(len(entries) == 1, f"expected exactly one '{APP_NAME}' install, found {entries}")
        inst = WindowsInstall(entries[0])
    elif IS_MAC:
        app = Path("/Applications") / f"{APP_NAME}.app"
        if app.exists():
            shutil.rmtree(app)  # what a drag-to-Applications "Replace" does
        if installer.name.endswith(".dmg"):
            mnt = Path(tempfile.mkdtemp(dir=work))
            run(["hdiutil", "attach", "-nobrowse", "-readonly", "-mountpoint", str(mnt), str(installer)])
            try:
                check(os.readlink(mnt / "Applications") == "/Applications", "DMG lacks /Applications")
                run(["ditto", str(mnt / f"{APP_NAME}.app"), str(app)])
            finally:
                subprocess.run(["hdiutil", "detach", str(mnt), "-force", "-quiet"], check=False)
        else:  # the updater's .app.tar.gz
            run(["tar", "-xzf", str(installer), "-C", "/Applications"])
        check(app.is_dir(), f"{app} missing after install")
        inst = Install()
        inst.app = app
        plist = app / "Contents" / "Info.plist"

        def pb(k):
            return subprocess.check_output(
                ["/usr/libexec/PlistBuddy", "-c", f"Print :{k}", str(plist)], text=True
            ).strip()

        check(pb("CFBundleIdentifier") == BUNDLE_ID, f"bundle id {pb('CFBundleIdentifier')}")
        inst.version = pb("CFBundleShortVersionString")
        inst.exe = app / "Contents" / "MacOS" / pb("CFBundleExecutable")
        if verify_sig:  # releases before 0.4 carry only the linker's signature
            run(["codesign", "--verify", "--deep", "--strict", str(app)])
    else:
        dest_dir = Path.home() / "Applications"
        dest_dir.mkdir(exist_ok=True)
        dest = dest_dir / "Soul-Player.AppImage"
        shutil.copy2(installer, dest)
        dest.chmod(0o755)
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


# ---------------------------------------------------------------- previous release


def fetch_previous(url: str, target_key: str, work: Path) -> tuple[str, Path] | None:
    try:
        with urllib.request.urlopen(url, timeout=60) as r:
            man = json.load(r)
    except Exception as e:  # noqa: BLE001 — no previous release is a normal state
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
    ap.add_argument(
        "--previous-manifest",
        help="URL of the currently published latest.json (default: none → upgrade/rollback skipped)",
    )
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
        uninstall(inst)
        steps.append(("fresh install → run → uninstall", "pass"))
        end_section()

        # 2 + 3. upgrade from, then roll back to, the previous release
        prev = fetch_previous(a.previous_manifest, a.target_key, work) if a.previous_manifest else None
        if prev is None:
            steps.append(("upgrade / rollback", "skipped (no previous build for this platform)"))
        else:
            prev_version, prev_installer = prev
            env = AppEnv(work, "upgrade")  # one profile: config carries across versions

            section(f"upgrade {prev_version} → {a.version}")
            old = install(prev_installer, work, verify_sig=False)
            # The previous release is history: if it can't run on today's runner that is
            # reported, not fatal — but installing over it, and getting back to it, are.
            try:
                live_run(old, env, prev_version, library, secs=10, require_log=False)
                prev_runs = True
            except E2EFailure as e:
                prev_runs = False
                log(f"::warning::previous release {prev_version} does not run on this runner: {e}")
                steps.append((f"previous {prev_version} runs here", "WARNING: no — see log"))
            if prev_runs and a.manifest and semver(prev_version) < semver(a.version):
                check_update_detected(old, env, a.manifest, a.version, library)
                steps.append((f"{prev_version} detects {a.version} as an update", "pass"))
            elif semver(prev_version) >= semver(a.version):
                log(f"::notice::{prev_version} is not older than {a.version}; update detection skipped")
            new = install(installer, work)
            check(new.version in ("", a.version), f"after upgrade the OS reports {new.version}")
            exercise(new, env, a.version, library, a.out / "upgraded", full=False)
            steps.append((f"upgrade {prev_version} → {a.version}", "pass"))
            end_section()

            section(f"rollback {a.version} → {prev_version}")
            uninstall(new)
            old = install(prev_installer, work, verify_sig=False)
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
    lines = [f"### Release e2e — {a.target_key} {a.version}", "", "| scenario | result |", "|---|---|"]
    lines += [f"| {s} | {r} |" for s, r in steps]
    text = "\n".join(lines) + "\n"
    log("\n" + text)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as f:
            f.write(text)


if __name__ == "__main__":
    raise SystemExit(main())
