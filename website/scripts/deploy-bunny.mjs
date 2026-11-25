// Publish dist/ to a Bunny storage zone and purge the pull zone in front of it.
//
// Bunny has no git integration and no atomic sync, so the order matters: upload
// everything first, delete what is no longer wanted second. Done the other way
// round, a visitor during the deploy gets a 404 instead of a stale page.
//
// Env (all required):
//   BUNNY_STORAGE_ZONE      storage zone name
//   BUNNY_STORAGE_PASSWORD  that zone's WRITE password (not the read-only one)
//   BUNNY_STORAGE_ENDPOINT  region host, e.g. storage.bunnycdn.com
//   BUNNY_PULLZONE_ID       numeric pull zone id
//   BUNNY_API_KEY           account API key, for the purge

import { readdir, readFile, stat } from 'node:fs/promises';
import { dirname, join, posix, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const DIST = resolve(HERE, '../dist');

const env = (name) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
};

const ZONE = env('BUNNY_STORAGE_ZONE');
const PASSWORD = env('BUNNY_STORAGE_PASSWORD');
const ENDPOINT = env('BUNNY_STORAGE_ENDPOINT');
const PULLZONE = env('BUNNY_PULLZONE_ID');
const API_KEY = env('BUNNY_API_KEY');

const STORAGE = `https://${ENDPOINT}/${ZONE}`;

/** Content types Bunny does not infer the way we want. */
const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.txt': 'text/plain; charset=utf-8',
  // Served as plain text so it is readable in a browser, which is what people
  // do before piping it to a shell.
  '.sh': 'text/plain; charset=utf-8',
};

const typeOf = (path) => {
  const dot = path.lastIndexOf('.');
  return (dot === -1 ? null : TYPES[path.slice(dot)]) ?? 'application/octet-stream';
};

/** Every file under `dir`, as paths relative to it, with forward slashes. */
async function walk(dir, base = dir) {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...(await walk(full, base)));
    else out.push(relative(base, full).split(/[\\/]/).join('/'));
  }
  return out;
}

/** Every file currently in the storage zone, recursively. */
async function remoteFiles(prefix = '') {
  const res = await fetch(`${STORAGE}/${prefix}`, { headers: { AccessKey: PASSWORD } });
  if (res.status === 404) return [];
  if (!res.ok) throw new Error(`listing /${prefix} failed: ${res.status} ${res.statusText}`);
  const entries = await res.json();
  const out = [];
  for (const entry of entries) {
    const path = posix.join(prefix, entry.ObjectName);
    if (entry.IsDirectory) out.push(...(await remoteFiles(`${path}/`)));
    else out.push(path);
  }
  return out;
}

async function upload(path) {
  const body = await readFile(join(DIST, path));
  const res = await fetch(`${STORAGE}/${path}`, {
    method: 'PUT',
    headers: { AccessKey: PASSWORD, 'Content-Type': typeOf(path) },
    body,
  });
  if (!res.ok) throw new Error(`upload ${path} failed: ${res.status} ${res.statusText}`);
  const { size } = await stat(join(DIST, path));
  console.log(`  up   ${path} (${size} bytes)`);
}

async function remove(path) {
  const res = await fetch(`${STORAGE}/${path}`, {
    method: 'DELETE',
    headers: { AccessKey: PASSWORD },
  });
  // A file another run already deleted is not an error worth failing on.
  if (!res.ok && res.status !== 404) {
    throw new Error(`delete ${path} failed: ${res.status} ${res.statusText}`);
  }
  console.log(`  del  ${path}`);
}

const local = await walk(DIST);
if (local.length === 0) throw new Error('dist/ is empty, refusing to wipe the zone');
console.log(`uploading ${local.length} files to ${ZONE}`);
for (const path of local) await upload(path);

const remote = await remoteFiles();
const stale = remote.filter((path) => !local.includes(path));
if (stale.length) {
  console.log(`removing ${stale.length} stale files`);
  for (const path of stale) await remove(path);
}

// The edge rules keep HTML and install.sh on short TTLs, but purging means a
// deploy is visible immediately rather than within the hour.
console.log('purging the pull zone');
const purge = await fetch(`https://api.bunny.net/pullzone/${PULLZONE}/purgeCache`, {
  method: 'POST',
  headers: { AccessKey: API_KEY, 'Content-Type': 'application/json' },
});
if (!purge.ok) throw new Error(`purge failed: ${purge.status} ${purge.statusText}`);

console.log('deployed');
