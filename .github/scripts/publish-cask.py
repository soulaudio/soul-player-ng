"""Publish the soul-player cask to soulaudio/homebrew-tap, tested before and after.

    publish-cask.py publish  --version 0.4.6 [--base main]   # after a release is verified
    publish-cask.py rollback --version 0.4.6 [--base main]   # take a version back out of the tap
    publish-cask.py drill                                     # both, for real, on a throwaway branch
    publish-cask.py --selftest

publish:
  1. Write the cask for the release's universal DMG (gen-cask.py; sha256 from the
     release's SHA256SUMS). Unchanged → done.
  2. Pre-flight on this Mac: the new cask from a local copy of the tap: style,
     audit, install (quarantined, right version), launch, zap. Nothing is live yet.
  3. A PR on the tap (branch + commit through the GitHub API, so the commit is the
     App's and signed). The tap's `cask-ci` (the same tests on a fresh Mac) must
     pass; then it merges. A failure closes the PR: nothing went live.
  4. Post-publish: install from the real tap as a user would. If that fails, the
     change is reverted through the same PR + cask-ci gate (rollback).

rollback: revert the version's commit on the base branch (a PR through cask-ci).
Reverting protects new installs; anyone who already upgraded needs a newer
release, since brew never downgrades.

drill: branches the tap from main (set back to before the newest release if the
tap already has it), publishes the newest release into the branch, rolls it
back, then offers a cask with a wrong checksum that cask-ci must reject: publish, rollback and the
gate itself, proven without touching main. The branch is deleted afterwards.

Needs GH_TOKEN with contents + pull-requests write on the tap (the release's
GitHub App token). The brew tests need macOS; `--no-brew` skips the local ones
(the tap's cask-ci still runs them on every PR).
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

APP_REPO = "soulaudio/soul-player-ng"
TAP = "soulaudio/homebrew-tap"
TAP_NAME = "soulaudio/tap"
CASK = "soul-player"
CASK_PATH = "Casks/soul-player.rb"
APP = Path("/Applications/Soul Player.app")
ZAP = ["Library/Application Support/soul-player", "Library/Logs/Soul Player", "Library/Caches/soul-player"]
GEN = Path(__file__).with_name("gen-cask.py")


class Failed(Exception):
    pass


def log(msg: str) -> None:
    print(msg, flush=True)


# ---------------------------------------------------------------- GitHub API


def api(method: str, path: str, body: dict | None = None, ok404: bool = False):
    req = urllib.request.Request(
        f"https://api.github.com/{path.lstrip('/')}",
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={
            "Authorization": f"Bearer {os.environ['GH_TOKEN']}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "soul-player-release",
        },
    )
    for attempt in range(5):
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                data = r.read()
                return json.loads(data) if data else {}
        except urllib.error.HTTPError as e:
            if e.code == 404 and ok404:
                return None
            if e.code >= 500 and attempt < 4:
                time.sleep(5 * (attempt + 1))
                continue
            raise Failed(f"{method} {path}: {e.code} {e.read().decode(errors='replace')[:500]}") from None
        except urllib.error.URLError:
            if attempt == 4:
                raise
            time.sleep(5 * (attempt + 1))
    raise Failed(f"{method} {path}: gave up")


def head_sha(branch: str) -> str:
    return api("GET", f"repos/{TAP}/git/ref/heads/{branch}")["object"]["sha"]


def cask_at(ref: str) -> tuple[str | None, str | None]:
    """(text, blob sha) of the cask at `ref`, or (None, None) when there is none."""
    f = api("GET", f"repos/{TAP}/contents/{CASK_PATH}?ref={ref}", ok404=True)
    if not f:
        return None, None
    return base64.b64decode(f["content"]).decode(), f["sha"]


def version_of(cask: str | None) -> str | None:
    m = re.search(r'^  version "([^"]+)"', cask or "", re.M)
    return m.group(1) if m else None


def write_branch(base: str, branch: str, text: str | None, message: str) -> str:
    """Branch `branch` off `base` with the cask set to `text` (None deletes it).
    Returns the new head sha."""
    api("POST", f"repos/{TAP}/git/refs", {"ref": f"refs/heads/{branch}", "sha": head_sha(base)})
    _, blob = cask_at(branch)
    if text is None:
        r = api("DELETE", f"repos/{TAP}/contents/{CASK_PATH}",
                {"message": message, "sha": blob, "branch": branch})
    else:
        body = {"message": message, "branch": branch,
                "content": base64.b64encode(text.encode()).decode()}
        if blob:
            body["sha"] = blob
        r = api("PUT", f"repos/{TAP}/contents/{CASK_PATH}", body)
    return r["commit"]["sha"]


def wait_check(sha: str, name: str = "cask-ci", timeout: int = 45 * 60) -> None:
    """Wait for the `name` check on `sha` to finish; raise unless it succeeded.

    A run GitHub cancels because no macOS runner was free ("not acquired by
    Runner … capacity constraints") is not a verdict on the cask: rerun it, up
    to three times."""
    deadline = time.time() + timeout
    appear_by = time.time() + 10 * 60
    reruns = 0
    while time.time() < deadline:
        runs = api("GET", f"repos/{TAP}/commits/{sha}/check-runs?check_name={name}")["check_runs"]
        if not runs and time.time() > appear_by:
            raise Failed(f"no {name} check started on {sha[:7]} within 10 min")
        if runs and all(r["status"] == "completed" for r in runs):
            r = max(runs, key=lambda r: r["completed_at"])
            if r["conclusion"] == "success":
                log(f"  {name} passed: {r['html_url']}")
                return
            run_id = re.search(r"/actions/runs/(\d+)", r.get("details_url") or "")
            if r["conclusion"] == "cancelled" and run_id and reruns < 3:
                reruns += 1
                log(f"  {name} was cancelled (no runner?): rerun {reruns}/3")
                api("POST", f"repos/{TAP}/actions/runs/{run_id.group(1)}/rerun-failed-jobs")
                time.sleep(30)
                continue
            raise Failed(f"{name} {r['conclusion']}: {r['html_url']}")
        time.sleep(20)
    raise Failed(f"{name} didn't finish on {sha[:7]} within {timeout // 60} min")


def pr_through_ci(base: str, text: str | None, title: str, body: str) -> str:
    """Change the cask on `base` through a PR that must pass cask-ci. Returns the
    merge commit. On a failed check the PR is closed and its branch deleted."""
    branch = f"cask/{re.sub(r'[^A-Za-z0-9.-]+', '-', title.lower())}-{int(time.time())}"
    head = write_branch(base, branch, text, title)
    pr = api("POST", f"repos/{TAP}/pulls", {"title": title, "head": branch, "base": base, "body": body})
    log(f"  PR {pr['html_url']}")
    try:
        wait_check(head)
    except Failed:
        api("PATCH", f"repos/{TAP}/pulls/{pr['number']}", {"state": "closed"})
        api("DELETE", f"repos/{TAP}/git/refs/heads/{branch}", ok404=True)
        log("  closed the PR and deleted its branch: nothing went live")
        raise
    merged = api("PUT", f"repos/{TAP}/pulls/{pr['number']}/merge",
                 {"merge_method": "squash", "commit_title": f"{title} (#{pr['number']})"})
    api("DELETE", f"repos/{TAP}/git/refs/heads/{branch}", ok404=True)
    log(f"  merged into {base}: {merged['sha'][:7]}")
    return merged["sha"]


# ---------------------------------------------------------------- brew tests


def brew(*args: str, env: dict | None = None, check: bool = True) -> str:
    r = subprocess.run(["brew", *args], capture_output=True, text=True, env=env, timeout=900)
    if check and r.returncode != 0:
        raise Failed(f"brew {' '.join(args)}:\n{r.stdout[-2000:]}\n{r.stderr[-2000:]}")
    return r.stdout


def brew_env() -> dict:
    e = dict(os.environ, HOMEBREW_NO_AUTO_UPDATE="1", HOMEBREW_NO_INSTALL_FROM_API="1",
             HOMEBREW_NO_ANALYTICS="1", HOMEBREW_NO_ENV_HINTS="1")
    e.setdefault("HOMEBREW_GITHUB_API_TOKEN", os.environ.get("GH_TOKEN", ""))
    return e


def install_check(version: str, env: dict) -> None:
    """Install soulaudio/tap/soul-player as a user would; check it's `version`,
    quarantined, launches; then zap it to nothing."""
    q = f"{TAP_NAME}/{CASK}"
    brew("install", "--cask", q, env=env)
    plist = APP / "Contents" / "Info.plist"

    def pb(key: str) -> str:
        return subprocess.check_output(["/usr/libexec/PlistBuddy", "-c", f"Print :{key}", str(plist)],
                                       text=True).strip()

    got = pb("CFBundleShortVersionString")
    if got != version:
        raise Failed(f"installed {got}, expected {version}")
    attrs = subprocess.run(["xattr", str(APP)], capture_output=True, text=True).stdout
    if "com.apple.quarantine" not in attrs:
        raise Failed("the installed app isn't quarantined: users would get a different app")
    exe = APP / "Contents" / "MacOS" / pb("CFBundleExecutable")
    subprocess.run(["xattr", "-dr", "com.apple.quarantine", str(APP)], check=False)
    p = subprocess.Popen([str(exe)], env=dict(env, SOUL_PLAYER_NO_UPDATE="1"),
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(10)
    if p.poll() is not None:
        raise Failed(f"the app exited at launch ({p.returncode})")
    p.terminate()
    p.wait(timeout=60)
    home = Path(env.get("HOME", str(Path.home())))
    brew("uninstall", "--cask", "--zap", q, env=env)
    left = [str(home / z) for z in ZAP if (home / z).exists()]
    if APP.exists() or left:
        raise Failed(f"zap left {[str(APP)] if APP.exists() else []} {left}")
    log(f"  ok: {version} installs from {TAP_NAME} (quarantined), launches, zaps to nothing")


def tap_from(source: str, branch: str, env: dict) -> None:
    """(Re)tap soulaudio/tap from GitHub (`source` = "github") at `branch`, or from
    a local checkout directory, and trust it."""
    brew("untap", "--force", TAP_NAME, env=env, check=False)
    if source == "github":
        brew("tap", TAP_NAME, env=env)
        repo = brew("--repository", TAP_NAME, env=env).strip()
        if branch != "main":
            subprocess.run(["git", "-C", repo, "fetch", "-q", "origin", branch], check=True)
            subprocess.run(["git", "-C", repo, "checkout", "-q", "FETCH_HEAD"], check=True)
    else:
        target = Path(brew("--repository", env=env).strip()) / "Library/Taps/soulaudio/homebrew-tap"
        shutil.rmtree(target, ignore_errors=True)
        shutil.copytree(source, target)
    brew("trust", TAP_NAME, env=env)


def preflight(text: str, version: str, env: dict) -> None:
    with tempfile.TemporaryDirectory() as d:
        (Path(d) / "Casks").mkdir()
        (Path(d) / CASK_PATH).write_text(text)
        tap_from(d, "main", env)
        brew("style", "--cask", f"{TAP_NAME}/{CASK}", env=env)
        # Not --new: that adds the Gatekeeper check an un-notarized app fails by design.
        brew("audit", "--cask", "--strict", "--online", f"{TAP_NAME}/{CASK}", env=env)
        install_check(version, env)
    brew("untap", "--force", TAP_NAME, env=env, check=False)


def postpublish(version: str, base: str, env: dict) -> None:
    tap_from("github", base, env)
    try:
        install_check(version, env)
    finally:
        brew("untap", "--force", TAP_NAME, env=env, check=False)


# ---------------------------------------------------------------- modes


def release_sha256(version: str) -> str:
    url = f"https://github.com/{APP_REPO}/releases/download/v{version}/SHA256SUMS"
    with urllib.request.urlopen(url, timeout=60) as r:
        sums = r.read().decode()
    return sha_for(sums, f"Soul-Player_{version}_universal.dmg")


def sha_for(sums: str, name: str) -> str:
    for line in sums.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == name:
            return parts[0]
    raise Failed(f"{name} isn't in SHA256SUMS")


def new_cask(version: str, sha: str) -> str:
    return subprocess.check_output([sys.executable, str(GEN), "--version", version, "--sha256", sha], text=True)


def publish(version: str, base: str, use_brew: bool) -> None:
    log(f"publish soul-player {version} → {TAP}@{base}")
    current, _ = cask_at(base)
    text = new_cask(version, release_sha256(version))
    if current == text:
        log("  the tap already has this cask")
        return
    env = brew_env()
    if use_brew:
        preflight(text, version, env)
    merged = pr_through_ci(base, text, f"soul-player {version}",
                           f"Soul Player {version}: https://github.com/{APP_REPO}/releases/tag/v{version}\n\n"
                           f"Opened by the release workflow after the release was published and verified; "
                           f"pre-flight tested on a Mac. Merges when `cask-ci` passes.")
    if not use_brew:
        return
    try:
        postpublish(version, base, env)
    except Failed as e:
        log(f"  post-publish check failed: {e}\n  rolling back")
        revert(merged, base, f"Revert soul-player {version}", str(e))
        raise


def revert(merged: str, base: str, title: str, why: str) -> None:
    parent = api("GET", f"repos/{TAP}/commits/{merged}")["parents"][0]["sha"]
    before, _ = cask_at(parent)
    pr_through_ci(base, before, title,
                  f"Restores the cask from before {merged[:7]}.\n\nReason: {why}\n\n"
                  "Opened by the release workflow. Installs that already took the reverted "
                  "version keep it until a newer release (brew never downgrades).")


def rollback(version: str, base: str) -> str | None:
    """Revert `version`'s commit on `base`. Returns the version now in the tap."""
    log(f"rollback soul-player {version} on {TAP}@{base}")
    text, _ = cask_at(base)
    if version_of(text) != version:
        log(f"  the tap has {version_of(text)}, not {version}: nothing to roll back")
        return version_of(text)
    commits = api("GET", f"repos/{TAP}/commits?sha={base}&path={CASK_PATH}&per_page=20")
    bump = next((c["sha"] for c in commits
                 if re.match(rf"soul-player {re.escape(version)}\b", c["commit"]["message"])), None)
    if not bump:
        raise Failed(f"no 'soul-player {version}' commit on {base}")
    revert(bump, base, f"Revert soul-player {version}", "rolled back by the release workflow")
    now = version_of(cask_at(base)[0])
    log(f"  the tap now has {now or 'no cask'}")
    return now


def drill(use_brew: bool) -> None:
    """Publish + roll back the newest release on a throwaway branch of the tap."""
    # The newest release, not whatever the tap holds: livecheck (and so the
    # pre-flight audit) only accepts the newest one, and that is what a release
    # publishes. The tap may lag behind it (a release before this pipeline).
    version = api("GET", f"repos/{APP_REPO}/releases/latest")["tag_name"].removeprefix("v")
    text, _ = cask_at("main")
    # Branch from main as it is now, so the PRs run today's cask-ci.
    branch = f"drill/{int(time.time())}"
    api("POST", f"repos/{TAP}/git/refs", {"ref": f"refs/heads/{branch}", "sha": head_sha("main")})
    before = version_of(text)
    if before == version:
        # Already published: set the branch back to the cask before this version.
        commits = api("GET", f"repos/{TAP}/commits?sha=main&path={CASK_PATH}&per_page=20")
        bump = next(c for c in commits if re.match(rf"soul-player {re.escape(version)}\b", c["commit"]["message"]))
        previous = cask_at(bump["parents"][0]["sha"])[0]
        before = version_of(previous)
        _, blob = cask_at(branch)
        message = f"drill: start from {before or 'no cask'}"
        if previous is None:
            api("DELETE", f"repos/{TAP}/contents/{CASK_PATH}", {"message": message, "sha": blob, "branch": branch})
        else:
            api("PUT", f"repos/{TAP}/contents/{CASK_PATH}", {
                "message": message, "sha": blob, "branch": branch,
                "content": base64.b64encode(previous.encode()).decode()})
    log(f"drill on {branch}: {before or 'no cask'} → publish {version} → roll back")
    try:
        publish(version, branch, use_brew)
        got = version_of(cask_at(branch)[0])
        if got != version:
            raise Failed(f"after publishing, {branch} has {got}")
        now = rollback(version, branch)
        if now != before:
            raise Failed(f"after rolling back, {branch} has {now}, expected {before}")
        if use_brew and before:
            postpublish(before, branch, brew_env())
        # The gate itself: a cask with a wrong checksum must be stopped by
        # cask-ci, its PR closed, and nothing land.
        bad = new_cask(version, "0" * 64)
        try:
            pr_through_ci(branch, bad, f"drill broken soul-player {version}",
                          "Deliberately wrong sha256: cask-ci must reject this and it must never merge.")
        except Failed as e:
            log(f"  ok: cask-ci rejected the broken cask ({e})")
        else:
            raise Failed("cask-ci let a cask with a wrong checksum merge")
        if version_of(cask_at(branch)[0]) != before:
            raise Failed("the rejected cask changed the branch anyway")
        log(f"DRILL PASSED: published {version} through cask-ci, rolled back to {before or 'no cask'}, "
            "and cask-ci stopped a broken cask")
    finally:
        api("DELETE", f"repos/{TAP}/git/refs/heads/{branch}", ok404=True)


def selftest() -> None:
    sums = "aa  Soul-Player_1.2.3_x86_64.dmg\nbb  Soul-Player_1.2.3_universal.dmg\n"
    assert sha_for(sums, "Soul-Player_1.2.3_universal.dmg") == "bb"
    try:
        sha_for(sums, "nope.dmg")
        raise AssertionError("accepted a missing file")
    except Failed:
        pass
    assert version_of('cask "x" do\n  version "1.2.3"\n') == "1.2.3"
    assert version_of(None) is None
    print("publish-cask selftest ok")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("mode", nargs="?", choices=["publish", "rollback", "drill"])
    ap.add_argument("--version")
    ap.add_argument("--base", default="main")
    ap.add_argument("--no-brew", action="store_true", help="skip the local brew tests (not on macOS)")
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.selftest:
        selftest()
        return 0
    use_brew = not a.no_brew
    try:
        if a.mode == "publish":
            publish(a.version, a.base, use_brew)
        elif a.mode == "rollback":
            rollback(a.version, a.base)
        elif a.mode == "drill":
            drill(use_brew)
        else:
            ap.error("a mode is required")
    except Failed as e:
        print(f"::error::{e}", flush=True)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
