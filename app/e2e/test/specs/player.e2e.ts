import path from 'path';

const FIXTURE = path.resolve(process.cwd(), '../../testdata').replace(/\\/g, '/');

describe('soul-player smoke', () => {
  it('renders the shell', async () => {
    await expect($('body')).toBeExisting();
  });

  it('loads a folder and shows tracks + transport', async () => {
    await browser.execute((dir) => {
      // @ts-ignore — page context
      return window.__TAURI_INTERNALS__.invoke('load_folder_path', { dir });
    }, FIXTURE);

    // Freeze on Repeat::One. Under SOUL_PLAYER_OFFLINE, the sink is a
    // backpressure-free NullSink (soul-player/src/offline.rs) — the whole
    // fixture queue decodes and "plays" in a fraction of a millisecond. With
    // the default Repeat::Off, `TwoLevelQueue::advance` clears `cur` to
    // `None` the instant a non-repeating source runs out
    // (soul-audio-queue/src/queue.rs), so by the time WebDriver's next poll
    // lands, now-playing has already reverted to nothing — unobservable, not
    // a real bug. Repeat::One freezes `cur` on natural end instead of
    // clearing it, giving the assertions below a stable target.
    await browser.execute(() => {
      // @ts-ignore — page context
      return window.__TAURI_INTERNALS__.invoke('send_command', { cmd: { SetRepeat: 'One' } });
    });

    // `load_folder_path` (like `open_folder`) only returns the scanned views
    // to its caller — it doesn't push them into the Svelte store, and the
    // "player" event channel deliberately never carries the library (see
    // store.svelte.ts). The app's own hydration path (`bindPlayerEvents`,
    // run from `App.svelte`'s `onMount`) re-fetches `library_views` on
    // mount ("fast path after re-open"). A webview reload re-runs that
    // mount against the same in-process backend state (the scan we just
    // injected), which is the same path a real re-open of the app takes.
    await browser.refresh();

    // Library rows appear (adapt the selector to Library.svelte's markup).
    await browser.waitUntil(async () => (await $$('[data-testid="track-row"]')).length > 0, {
      timeout: 10000,
      timeoutMsg: 'no track rows rendered after load_folder_path',
    });

    // Now-playing title shows in the player bar (adapt selector).
    await expect($('[data-testid="now-playing-title"]')).toBeExisting();
  });

  it('clicking a row changes now-playing', async () => {
    const rows = await $$('[data-testid="track-row"]');
    const title = await $('[data-testid="now-playing-title"]');
    const before = await title.getText();
    await rows[rows.length - 1].click();
    await browser.waitUntil(async () => (await title.getText()) !== before, { timeout: 5000 });
  });
});

// Explicit per-format coverage for the two MPEG audio layers the player claims
// to support but that are otherwise only implicit rows in the mixed-format
// smoke scan above. Uses a dedicated two-file fixture folder whose stems
// ("layer-1" / "layer-2") render as unique track titles — the mixed testdata/
// scan can't single them out because every tagless `tone.*` file falls back to
// the same "tone" stem. The `.mp1` fixture is core's Layer I conformance vector
// (ffmpeg has no Layer I encoder); the `.mp2` is the committed Layer II tone.
describe('MPEG layer-1 / layer-2 playback', () => {
  const MPEG_FIXTURE = path.resolve(process.cwd(), 'fixtures/mpeg-layers').replace(/\\/g, '/');

  it('scans and lists both an MP1 (Layer I) and MP2 (Layer II) track', async () => {
    await browser.execute((dir) => {
      // @ts-ignore — page context
      return window.__TAURI_INTERNALS__.invoke('load_folder_path', { dir });
    }, MPEG_FIXTURE);
    // Freeze now-playing on natural end (see the smoke test above for why).
    await browser.execute(() => {
      // @ts-ignore — page context
      return window.__TAURI_INTERNALS__.invoke('send_command', { cmd: { SetRepeat: 'One' } });
    });
    await browser.refresh();

    // Exactly the two MPEG-layer fixtures — nothing else in that folder.
    await browser.waitUntil(async () => (await $$('[data-testid="track-row"]')).length === 2, {
      timeout: 10000,
      timeoutMsg: 'expected exactly the two MPEG-layer rows',
    });

    const titles = await $$('[data-testid="track-row"] .title').map((el) => el.getText());
    expect(titles).toContain('layer-1'); // the .mp1 (Layer I) fixture
    expect(titles).toContain('layer-2'); // the .mp2 (Layer II) fixture
  });

  it('plays the MP2 track when its row is clicked', async () => {
    // load_folder auto-plays track 0 (sorted: layer-1.mp1), so the MP1 is
    // already the now-playing track — clicking the MP2 row proves Layer II
    // playback switches independently.
    const titles = await $$('[data-testid="track-row"] .title').map((el) => el.getText());
    const rows = await $$('[data-testid="track-row"]');
    const mp2Index = titles.indexOf('layer-2');
    expect(mp2Index).toBeGreaterThanOrEqual(0);

    const npTitle = $('[data-testid="now-playing-title"]');
    await rows[mp2Index].click();
    await browser.waitUntil(async () => (await npTitle.getText()) === 'layer-2', {
      timeout: 5000,
      timeoutMsg: 'now-playing did not switch to the MP2 (layer-2) track',
    });
  });
});
