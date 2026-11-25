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

Needs two credentials, both from the infrastructure repo's `.env`:
`BUNNYNET_API_KEY` (account-level, for Bunny) and
`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (the scoped Scaleway Object
Storage key, for the state backend).

```sh
cd website/infrastructure

# The DNS zone id comes from the infrastructure repo:
#   terraform -chdir=dns/soulaudio.co output zone_id
export TF_VAR_zone_id=...

terraform init -backend-config=backend.hcl
terraform plan
terraform apply
```

`terraform fmt -check -recursive` and `terraform validate` both pass; the
config is checked under Terraform and OpenTofu.

## After the first apply

The outputs are exactly the repository secrets
`.github/workflows/site.yml` needs. Until they are set, the workflow builds the
site on every push and skips publishing with a notice.

| Secret | Where it comes from |
|---|---|
| `BUNNY_STORAGE_ZONE` | `terraform output -raw storage_zone` |
| `BUNNY_STORAGE_PASSWORD` | `terraform output -raw storage_password` (the **write** password; the read-only one cannot upload) |
| `BUNNY_STORAGE_ENDPOINT` | `terraform output -raw storage_endpoint` |
| `BUNNY_PULLZONE_ID` | `terraform output -raw pullzone_id` |
| `BUNNY_API_KEY` | the Bunny account API key, not a terraform output |

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
