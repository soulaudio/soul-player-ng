# player.soulaudio.co: the download page's CDN, Bunny-native.
#
# Storage zone holds the build, a pull zone fronts it, player.soulaudio.co
# points at the pull zone. The DNS ZONE itself lives in the infrastructure
# repo (dns/soulaudio.co); the CDN lives here, with the site it serves. That
# split is the house pattern, and dns/soulaudio.co/main.tf says so in as many
# words: "the pull zone itself is managed in the soulaudio web project".
#
# The record below is a subdomain in that zone, created from here by id. The
# apex and www records stay in the DNS slice, pointing at a different pull
# zone (the marketing site).
#
# .github/workflows/site.yml uploads the build into the storage zone and
# purges this pull zone. The secrets it needs come out of outputs.tf.

resource "bunnynet_storage_zone" "player" {
  name      = var.site_name
  region    = var.storage_region
  zone_tier = "Standard"
}

resource "bunnynet_pullzone" "player" {
  name = var.site_name

  origin {
    type        = "StorageZone"
    storagezone = bunnynet_storage_zone.player.id
  }

  routing {
    tier = "Standard"
  }

  # A storage origin otherwise emits a ~296-day max-age, which would pin the
  # download page to whatever version was current when someone first loaded
  # it. Five minutes means a deploy self-heals even if the purge fails; the
  # edge rules then stretch that back out for the immutable assets.
  cache_enabled                 = true
  cache_expiration_time         = 300
  cache_expiration_time_browser = 300
}

resource "bunnynet_dns_record" "player" {
  zone        = var.zone_id
  name        = "player"
  type        = "PullZone"
  value       = bunnynet_pullzone.player.name
  pullzone_id = bunnynet_pullzone.player.id
  ttl         = 300
}

# depends_on orders the two operations but cannot wait for the record to be
# visible, and a first apply tried to issue the certificate two seconds after
# creating the record: "The domain player.soulaudio.co is not pointing to our
# servers". A re-run worked, which is the tell. The wait makes one apply
# enough.
resource "time_sleep" "dns_propagation" {
  depends_on      = [bunnynet_dns_record.player]
  create_duration = "90s"
}

resource "bunnynet_pullzone_hostname" "player" {
  pullzone    = bunnynet_pullzone.player.id
  name        = var.hostname
  tls_enabled = true
  force_ssl   = true

  depends_on = [time_sleep.dns_propagation]
}
