# These five values are exactly the repository secrets
# .github/workflows/site.yml needs. After an apply:
#
#   terraform output -raw storage_zone        -> BUNNY_STORAGE_ZONE
#   terraform output -raw storage_password    -> BUNNY_STORAGE_PASSWORD
#   terraform output -raw storage_endpoint    -> BUNNY_STORAGE_ENDPOINT
#   terraform output -raw pullzone_id         -> BUNNY_PULLZONE_ID
#   (BUNNY_API_KEY is the account key, not a terraform output)

output "storage_zone" {
  description = "Storage zone name the deploy uploads into."
  value       = bunnynet_storage_zone.player.name
}

output "storage_password" {
  description = "Storage zone WRITE password. This is the one the deploy needs; the read-only password cannot upload."
  value       = bunnynet_storage_zone.player.password
  sensitive   = true
}

output "storage_endpoint" {
  description = "Storage API host for the zone's region. DE (Falkenstein) answers on the unprefixed host; other regions take a prefix (ny., la., sg., syd., uk., se., br., jh.)."
  value       = var.storage_region == "DE" ? "storage.bunnycdn.com" : "${lower(var.storage_region)}.storage.bunnycdn.com"
}

output "pullzone_id" {
  description = "Pull zone id, for the cache purge at the end of a deploy."
  value       = bunnynet_pullzone.player.id
}

output "pullzone_name" {
  description = "Pull zone name, which also forms <name>.b-cdn.net."
  value       = bunnynet_pullzone.player.name
}

output "hostname" {
  description = "Where the site answers once the certificate is issued."
  value       = "https://${var.hostname}"
}
