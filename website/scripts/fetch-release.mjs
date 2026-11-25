// Resolve the latest release at BUILD time and write it to src/data/release.json.
//
// Doing this at build time rather than in the browser means no GitHub API call
// per visitor (and no rate limit), and the page ships as static HTML with real
// URLs in it. The cost is that the site must be redeployed when a release
// publishes, which release.yml triggers.
//
// Asset filenames carry the version, so they are matched by shape, not spelled
// out. If the release pipeline renames an asset, the matching `download` entry
// goes missing and the build fails loudly rather than shipping a dead button.

import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = resolve(HERE, '../src/data/release.json');
const REPO = process.env.SOUL_PLAYER_REPO ?? 'soulaudio/soul-player-ng';

/** Each download the page offers, and how to recognise its asset. */
const WANTED = [
  { id: 'windows-x86_64', os: 'Windows', label: '.exe installer', arch: 'x86_64', match: (n) => n.endsWith('-setup.exe') },
  { id: 'macos-universal', os: 'macOS', label: '.dmg (universal)', arch: 'Apple silicon + Intel', match: (n) => n.endsWith('_universal.dmg') },
  { id: 'linux-deb-x86_64', os: 'Linux', label: '.deb', arch: 'x86_64', match: (n) => n.endsWith('_amd64.deb') },
  { id: 'linux-deb-aarch64', os: 'Linux', label: '.deb', arch: 'aarch64', match: (n) => n.endsWith('_arm64.deb') },
  { id: 'linux-rpm-x86_64', os: 'Linux', label: '.rpm', arch: 'x86_64', match: (n) => n.endsWith('.x86_64.rpm') },
  { id: 'linux-rpm-aarch64', os: 'Linux', label: '.rpm', arch: 'aarch64', match: (n) => n.endsWith('.aarch64.rpm') },
  { id: 'linux-appimage-x86_64', os: 'Linux', label: '.AppImage', arch: 'x86_64', match: (n) => n.endsWith('_x86_64.AppImage') },
  { id: 'linux-appimage-aarch64', os: 'Linux', label: '.AppImage', arch: 'aarch64', match: (n) => n.endsWith('_aarch64.AppImage') },
  { id: 'linux-flatpak-x86_64', os: 'Linux', label: 'Flatpak bundle', arch: 'x86_64', match: (n) => n.endsWith('_x86_64.flatpak') },
  { id: 'linux-flatpak-aarch64', os: 'Linux', label: 'Flatpak bundle', arch: 'aarch64', match: (n) => n.endsWith('_aarch64.flatpak') },
];

const headers = { Accept: 'application/vnd.github+json', 'User-Agent': 'soul-player-website' };
if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;

const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers });
if (!res.ok) {
  throw new Error(`GitHub returned ${res.status} ${res.statusText} for the latest release of ${REPO}`);
}
const release = await res.json();

const downloads = [];
const missing = [];
for (const want of WANTED) {
  const asset = release.assets.find((a) => want.match(a.name));
  if (!asset) {
    missing.push(want.id);
    continue;
  }
  downloads.push({
    id: want.id,
    os: want.os,
    label: want.label,
    arch: want.arch,
    name: asset.name,
    url: asset.browser_download_url,
    size: asset.size,
  });
}

if (missing.length) {
  throw new Error(
    `release ${release.tag_name} has no asset matching: ${missing.join(', ')}. ` +
      `Either the pipeline renamed something, or the release is incomplete.`,
  );
}

const data = {
  version: release.tag_name.replace(/^v/, ''),
  tag: release.tag_name,
  published: release.published_at,
  releaseUrl: release.html_url,
  checksums: release.assets.find((a) => a.name === 'SHA256SUMS')?.browser_download_url ?? null,
  downloads,
};

await mkdir(dirname(OUT), { recursive: true });
await writeFile(OUT, `${JSON.stringify(data, null, 2)}\n`);
console.log(`release ${data.tag}: ${downloads.length} downloads -> src/data/release.json`);
