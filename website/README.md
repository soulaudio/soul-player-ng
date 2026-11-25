# website

[player.soulaudio.co](https://player.soulaudio.co): one page, showing what Soul
Player is and where to download it, plus the `install.sh` the page tells people
to pipe into a shell.

Astro, static output, one small script for the platform switcher. Hosted on
Bunny, deployed by `.github/workflows/site.yml`.

```
website/
  src/              the page
  public/           fonts, and the generated copies of install.sh + screenshot
  scripts/          build-time release lookup, asset copy, Bunny deploy
  infrastructure/   terraform: storage zone, pull zone, DNS record, edge rules
```

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

The page is built from the current release and deployed by the release, so it
is 1:1 by construction rather than by reconciliation. Two triggers:

| Trigger | Covers |
|---|---|
| `release.yml` calls this workflow once a release verifies | a new version |
| Pushes touching `website/**` or `packaging/install.sh` | the page itself changing |

There is deliberately no scheduled poll. A cron comparing the live site
against the latest release would only ever catch a case the release path was
supposed to handle, which is a reason to fix that path rather than to watch
it. The release path checks itself instead:

- After deploying, the workflow re-reads `/version.json` from the live URL
  with a cache-busting query and fails if it is not the version just built. An
  upload that succeeded while the edge kept serving the old HTML is otherwise
  invisible.
- The installer matrix then installs from the deployed `install.sh` on nine
  systems, and **a failure there rolls the release back**: the release is
  demoted, the previous one becomes latest again, and `site-after-rollback`
  rebuilds this page so it follows.

One case the automation does not handle: a release published while the
credentials are missing. The workflow says so and does not fail.

It needs three secrets and one variable, set on the **`production`
environment** (Settings -> Environments -> production), not at repository
level: a pull request then cannot reach them. Nothing has to be copied out of
a terraform output by hand, because the deploy reads the zone name, its write
password, and the pull zone id straight from the state that created them.

| Setting | Kind | What it is |
|---|---|---|
| `BUNNYNET_API_KEY` | secret | Bunny account key. The terraform provider authenticates with it, and the deploy purges the cache with it. |
| `SCW_STATE_ACCESS_KEY` | secret | Scaleway Object Storage key id, for the terraform state backend. |
| `SCW_STATE_SECRET_KEY` | secret | Its secret. |
| `SOULAUDIO_ZONE_ID` | variable | The `soulaudio.co` Bunny DNS zone id. Not a secret: it is in every plan output. |

The same three secrets serve both workflows, so there is one set of
credentials to rotate rather than two.

### Bunny configuration

Not configured by hand: `infrastructure/` is terraform for the storage zone,
the pull zone, the `player` DNS record, and every edge rule (what is cached,
how long, the `install.sh` content type, the security headers). Its README
covers how it is applied (in CI) and what each rule is for.

The DNS *zone* is not here. It belongs to the infrastructure repo's
`dns/soulaudio.co` slice; only the `player` record is created from this
project, which is the same split `datadir.co` and `osakiseiichi.com` use.
