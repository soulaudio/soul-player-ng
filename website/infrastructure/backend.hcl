# Partial backend config for: terraform init -backend-config=backend.hcl
# Creds from AWS_ACCESS_KEY_ID/SECRET (the scoped Scaleway ObjectStorage key in
# the infrastructure repo's .env). Same bucket as every other slice, its own
# key: this state owns the player CDN and nothing else, so an apply here can
# never touch the DNS slice's resources.
bucket = "datadir-infra-tfstate"
key    = "soulaudio/player-site.tfstate"
region = "fr-par"

endpoints = { s3 = "https://s3.fr-par.scw.cloud" }

skip_credentials_validation = true
skip_region_validation      = true
skip_requesting_account_id  = true
skip_metadata_api_check     = true

use_lockfile = true
