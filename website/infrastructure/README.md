# infrastructure

Terraform for **player.soulaudio.co**: the Bunny storage zone the build is
uploaded into, the pull zone in front of it, the DNS record, and the edge rules
that decide what is cached and what headers go out.

## Why the CDN is here and the DNS zone is not

The `soulaudio.co` DNS **zone** belongs to the
[infrastructure repo](https://github.com/datadir/infrastructure)'s
`dns/soulaudio.co` slice, which also owns the apex and `www` records pointing
at the marketing site's pull zone. That slice says the split out loud: *"the
pull zone itself is managed in the soulaudio web project"*. Same shape as
`datadir.co` and `osakiseiichi.com`.

So the CDN lives with the site it serves, and only the `player` subdomain
record is created from here, into the existing zone, by id.

State is the shared Scaleway bucket (`datadir-infra-tfstate`) under its own
key, `soulaudio/player-site.tfstate`. Its own key matters: an apply here can
then never decide the DNS slice's resources are unmanaged and remove them,
which is the failure `dns/datadir-sites` was split out to avoid.

## Applying

In CI, not from a laptop. `.github/workflows/infrastructure.yml` plans on a
pull request that touches this directory and applies when it lands on main, so
the state in the shared bucket always matches what the repo says, and the
credentials live in one place instead of in whoever applied last.

Both this workflow and the site deploy take their credentials from the
**`production` environment** (Settings -> Environments -> production):

| Setting | Kind | What it is |
|---|---|---|
| `BUNNYNET_API_KEY` | secret | Bunny account key |
| `SCW_STATE_ACCESS_KEY` | secret | Scaleway Object Storage key id, for the state backend |
| `SCW_STATE_SECRET_KEY` | secret | Its secret |
| `SOULAUDIO_ZONE_ID` | variable | `soulaudio.co` zone id, from `terraform -chdir=dns/soulaudio.co output zone_id` in the infrastructure repo |

An environment rather than repository secrets, so a pull request run cannot
reach them. The consequence to know about: a PR gets fmt and validate but no
plan, because it has no credentials to plan with. The plan still runs on main,
in the same job as the apply, where it is the record of what was applied.

Adding required reviewers to the environment would gate an apply on approval.
Note that the approval happens before the job starts, so the reviewer would be
approving without having seen the plan; getting plan-then-approve means
splitting plan and apply into two jobs with the plan passed between them as an
artifact. Worth it for a team, probably not for one person.

`terraform fmt -check` and `validate` run on every trigger, credentials or
not, so a malformed change fails before it reaches a plan. Until the
credentials are set the workflow stops after those two and says so rather than
failing.

The plan is written into the run's job summary, so it is read before the merge
rather than after the apply.

Destroying is a `workflow_dispatch` with the `destroy` input, which keeps it
deliberate and off the automatic path.

### Running it locally anyway

For a break-glass case or to try something before committing it. Needs
`BUNNYNET_API_KEY` plus the Scaleway state key as `AWS_ACCESS_KEY_ID` /
`AWS_SECRET_ACCESS_KEY`, all from the infrastructure repo's `.env`:

```sh
cd website/infrastructure
export TF_VAR_zone_id=...   # terraform -chdir=dns/soulaudio.co output zone_id
terraform init -backend-config=backend.hcl
terraform plan
```

Prefer leaving the apply to CI: a local apply takes the same state lock and
the next pipeline run will plan against whatever you left behind.

## What the deploy does with the outputs

`.github/workflows/site.yml` reads `storage_zone`, `storage_password`,
`storage_endpoint` and `pullzone_id` from this state at deploy time (masking
the password before it can reach a log line). That is why there are no
`BUNNY_STORAGE_*` repository secrets: a zone recreated under a new name needs
no secret updated anywhere.

The TLS certificate is issued only once the DNS record resolves, which is why
`bunnynet_pullzone_hostname` depends on it. Give it a minute after the first
apply.

## What the edge rules do

The page has two kinds of content with opposite needs: `/_astro/*` and
`/fonts/*` are hashed or stable and cached for a year, while everything that
names the current release must never be stale.

| Path | Rule |
|---|---|
| `/_astro/*`, `/fonts/*` | one year at the edge |
| `/`, `*.html` | `max-age=300, must-revalidate` |
| `/version.json` | `no-store`, because the scheduled sync job reads it to decide whether the live site has fallen behind |
| `/install.sh` | served as `text/plain` so it is readable in a browser, and revalidating |
| everything | HSTS, a strict CSP, `nosniff`, `Referrer-Policy` |

The short HTML TTL is a backstop rather than the mechanism: the deploy purges
the zone. It matters because a storage origin otherwise emits a ~296-day
`max-age`, which would pin a visitor's browser to whatever release was current
when they first loaded the page, somewhere no purge can reach.

## If an apply fails on the zone name

Storage and pull zone names are globally unique across all of Bunny, and a
deleted name stays reserved for hours. If `soul-player-site` is ever taken or
stuck, set `-var site_name=soulaudio-player-site-2` and re-apply. The same
thing is why the datum site in the infrastructure repo lives at
`datadir-datum-site-5`.
