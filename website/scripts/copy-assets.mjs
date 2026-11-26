// Copy the two files the site serves but does not own, so each has exactly one
// source of truth in the repo:
//
//   packaging/install.sh             -> public/install.sh  (the curl | sh target)
//   docs/screenshots/albums-dark.png -> public/screenshot.png
//
// The favicon is no longer derived from the app icon: it is a purpose-built,
// theme-aware pair committed under public/ (favicon-light.png / favicon-dark.png).
//
// Copying at build time rather than committing duplicates means the served
// installer cannot drift from the one the repo tests, and the hero image cannot
// drift from the one in the README.

import { copyFile, mkdir } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, '../..');
const PUBLIC = resolve(HERE, '../public');

const FILES = [
  ['packaging/install.sh', 'install.sh'],
  ['docs/screenshots/albums-dark.png', 'screenshot.png'],
];

await mkdir(PUBLIC, { recursive: true });
for (const [from, to] of FILES) {
  await copyFile(resolve(ROOT, from), resolve(PUBLIC, to));
  console.log(`copied ${from} -> public/${to}`);
}
