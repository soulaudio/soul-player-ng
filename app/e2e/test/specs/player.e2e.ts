// End-to-end smoke + per-format coverage for the Tauri shell.
//
// The e2e build auto-loads `SOUL_PLAYER_E2E_FOLDER` at startup
// (commands::e2e_autoload) and freezes on Repeat::One, so the library is
// already present when the webview mounts. The suite therefore drives the app
// with ONLY native WebDriver DOM commands (findElement / click / getText) and
// never injects Tauri IPC via `browser.execute`: WebView2 runs
// WebDriver-injected scripts with an opaque origin, and Tauri's IPC handler
// rejects that ("Origin header is not a valid URL"). The frontend's own
// mount-time IPC runs in the page's real origin and works, which is what
// renders the rows below.

// The all-formats fixture folder holds one tagless file per SUPPORTED
// extension, named `fmt-<ext>` so each row's title (filename-stem fallback) is
// unique and assertable. Keep in sync with the folder and the SUPPORTED list
// in soul-audio-service/src/library.rs.
const EXPECTED_FORMATS = [
  'fmt-aac', 'fmt-aif', 'fmt-aiff', 'fmt-caf', 'fmt-dff', 'fmt-dsf',
  'fmt-flac', 'fmt-m4a', 'fmt-mka', 'fmt-mp1', 'fmt-mp2', 'fmt-mp3',
  'fmt-oga', 'fmt-ogg', 'fmt-opus', 'fmt-wav', 'fmt-webm', 'fmt-wv',
].sort();

const rowTitles = async (): Promise<string[]> =>
  $$('[data-testid="track-row"] .title').map((el) => el.getText());

describe('soul-player e2e', () => {
  it('renders the shell', async () => {
    await expect($('body')).toBeExisting();
  });

  it('renders the auto-loaded library (no WebDriver IPC)', async () => {
    // Rows appear only if the frontend's normal mount-time `library_views` IPC
    // succeeded against the backend state injected at startup — the heart of
    // the Origin-header fix.
    await browser.waitUntil(async () => (await $$('[data-testid="track-row"]')).length > 0, {
      timeout: 15000,
      timeoutMsg: 'no track rows rendered from the auto-loaded folder',
    });
    // Auto-play + Repeat::One freezes now-playing on the first track.
    await expect($('[data-testid="now-playing-title"]')).toBeExisting();
    await browser.waitUntil(async () => (await $('[data-testid="now-playing-title"]').getText()) !== 'No track', {
      timeout: 5000,
      timeoutMsg: 'now-playing never populated',
    });
  });

  it('lists every supported format exactly once (1:1)', async () => {
    const titles = (await rowTitles()).sort();
    expect(titles).toEqual(EXPECTED_FORMATS);
  });

  it('plays a clicked track — MP1 (Layer I) and MP2 (Layer II)', async () => {
    const npTitle = $('[data-testid="now-playing-title"]');
    for (const target of ['fmt-mp1', 'fmt-mp2']) {
      const titles = await rowTitles();
      const rows = await $$('[data-testid="track-row"]');
      const idx = titles.indexOf(target);
      expect(idx).toBeGreaterThanOrEqual(0);
      await rows[idx].click();
      await browser.waitUntil(async () => (await npTitle.getText()) === target, {
        timeout: 5000,
        timeoutMsg: `now-playing did not switch to ${target}`,
      });
    }
  });
});
