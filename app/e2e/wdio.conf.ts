import os from 'os';
import path from 'path';
import { spawn, spawnSync, type ChildProcess } from 'child_process';
import { fileURLToPath } from 'url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const APP = path.resolve(__dirname, '../src-tauri/target/debug/soul-player.exe');
// Folder the e2e build auto-loads at startup (commands::e2e_autoload). Injecting
// the library in-process avoids driving Tauri IPC through `browser.execute`,
// which WebView2 runs with an opaque origin that Tauri's IPC handler rejects.
const E2E_FOLDER = path.resolve(__dirname, 'fixtures/all-formats');
let tauriDriver: ChildProcess | undefined;
let exiting = false;

export const config: WebdriverIO.Config = {
  host: '127.0.0.1',
  port: 4444,
  specs: ['./test/specs/**/*.e2e.ts'],
  maxInstances: 1,
  capabilities: [{ maxInstances: 1, 'tauri:options': { application: APP } } as any],
  reporters: ['spec'],
  framework: 'mocha',
  mochaOpts: { ui: 'bdd', timeout: 120000 },
  autocompileOpts: { autoCompile: true, tsNodeOpts: { transpileOnly: true } },

  // Build the debug, unbundled binary WITH the e2e feature (load_folder_path)
  // and run the actor device-free.
  onPrepare: () => {
    process.env.SOUL_PLAYER_OFFLINE = '1';
    process.env.SOUL_PLAYER_E2E_FOLDER = E2E_FOLDER;
    const r = spawnSync(
      'npm',
      ['run', 'tauri', 'build', '--', '--debug', '--no-bundle', '--features', 'e2e'],
      { cwd: path.resolve(__dirname, '..'), stdio: 'inherit', shell: true },
    );
    if (r.status !== 0) throw new Error('tauri build failed');
  },
  beforeSession: () => {
    tauriDriver = spawn(path.resolve(os.homedir(), '.cargo', 'bin', 'tauri-driver'), [], {
      stdio: [null, process.stdout, process.stderr],
      env: { ...process.env, SOUL_PLAYER_OFFLINE: '1', SOUL_PLAYER_E2E_FOLDER: E2E_FOLDER },
    });
    tauriDriver.on('exit', (code) => {
      if (!exiting) { console.error('tauri-driver exited early:', code); process.exit(1); }
    });
  },
  afterSession: () => { exiting = true; tauriDriver?.kill(); },
};
