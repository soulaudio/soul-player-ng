# website

[player.soulaudio.co](https://player.soulaudio.co): one page, showing what Soul
Player is and where to download it, plus the `install.sh` the page tells people
to pipe into a shell.

Astro, static output, no client-side JavaScript. Hosted on Bunny (a storage zone
behind a pull zone), deployed by `.github/workflows/site.yml`.

## Local

```sh
cd website
npm ci
npm run dev        # http://localhost:4321
npm run build      # -> dist/
```

`npm run stage` (which `dev` and `build` both run first) does two things:

- **`scripts/fetch-release.mjs`** asks the GitHub API for the latest release and
  writes `src/data/release.json`. Asset filenames carry the version, so the page
  cannot hardcode them. If an expected asset is missing the build fails rather
  than shipping a dead download button. Set `GITHUB_TOKEN` to avoid the
  unauthenticated rate limit.
- **`scripts/copy-assets.mjs`** copies `packaging/install.sh` and
  `docs/screenshots/albums-dark.png` into `public/`. Both have one source of
  truth outside this directory, so neither is committed here.

Generated and gitignored: `src/data/release.json`, the served
`public/version.json`, and the copies at `public/install.sh`,
`public/screenshot.png`, and `public/favicon.png`.

## Staying in sync with the current release

The page is static, so it only shows the current version if something rebuilds
it. Three triggers, because any one of them can miss:

| Trigger | Covers | Lag |
|---|---|---|
| `release.yml` calls this workflow once a release verifies | the normal path | minutes |
| A six-hourly schedule | a failed deploy, a release edited or withdrawn by hand, anything that happened without a push | up to 6 hours |
| Pushes touching `website/**` or `packaging/install.sh` | the page itself changing | minutes |

The scheduled run does not redeploy blindly. The build writes `/version.json`
alongside the page, so the job can compare what the live site is serving
against the latest release and exit when they match. A quiet week costs one
cheap check every six hours and no cache purges.

After any deploy, the workflow re-reads `/version.json` from the live URL (with
a cache-busting query) and fails if it is not the version just built. An upload
that succeeded while the edge kept serving the old HTML is otherwise invisible,
and this turns it into a red run.

Two cases the automation deliberately does not handle: a rollback (`release.yml`
withdraws a failed release and restores the previous one as latest, which the
`site` job never ran for, so the page is already correct), and a release
published while the Bunny secrets are missing (the workflow says so and does not
fail).

It needs five repository secrets:

| Secret | What it is |
|---|---|
| `BUNNY_STORAGE_ZONE` | Storage zone name |
| `BUNNY_STORAGE_PASSWORD` | That zone's **write** password |
| `BUNNY_STORAGE_ENDPOINT` | Zone's region host, e.g. `storage.bunnycdn.com` |
| `BUNNY_PULLZONE_ID` | Numeric pull zone id, for the cache purge |
| `BUNNY_API_KEY` | Account API key, for the cache purge |

### Bunny configuration

Set up once, in the dashboard:

- A **storage zone** holding the built site, and a **pull zone** with that zone
  as its origin, with `player.soulaudio.co` as a custom hostname, a Let's
  Encrypt certificate, Force SSL, HTTP/3, and Brotli.
- **Edge rules** for caching, because `/_astro/*` is immutable but the HTML and
  the installer must never be stale:

  | Path | Edge cache | Browser cache |
  |---|---|---|
  | `/_astro/*` | 1 year | 1 year, immutable |
  | `/install.sh` | 5 minutes | 0 |
  | everything else | 1 hour | 0 |

  The deploy purges the zone anyway, so the short TTLs are a backstop. Browser
  cache 0 on the HTML is what stops a visitor seeing a stale version number out
  of their own cache, which purging the edge cannot fix.
- An edge rule serving `/install.sh` as `text/plain` so it is readable in a
  browser, which is what people do before piping it to a shell.
- Security headers: HSTS, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: strict-origin-when-cross-origin`, and a strict CSP. The page
  is static with no third-party resources, so nothing needs loosening.
