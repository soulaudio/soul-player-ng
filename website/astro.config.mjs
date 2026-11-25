import { defineConfig } from 'astro/config';

export default defineConfig({
  site: 'https://player.soulaudio.co',
  // Hashed, immutable output under /_astro/, which is what the Bunny edge rule
  // caches for a year. Everything else is served short and purged on deploy.
  // Styles and the platform switcher stay as their own files rather than
  // inlined, so the Content-Security-Policy needs no inline exception.
  build: { assets: '_astro', inlineStylesheets: 'never' },
  // Bunny serves a storage zone as plain files: a trailing-slash redirect would
  // be an extra round trip, and `file` keeps the output as /index.html.
  trailingSlash: 'never',
});
