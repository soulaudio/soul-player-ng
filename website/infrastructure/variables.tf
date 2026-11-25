variable "zone_id" {
  description = <<-EOT
    soulaudio.co Bunny DNS zone id, owned by the infrastructure repo's
    dns/soulaudio.co slice. Read it from there:

      terraform -chdir=dns/soulaudio.co output zone_id

    Required rather than defaulted: a wrong id would silently write the record
    into somebody else's zone.
  EOT
  type        = string
}

variable "site_name" {
  description = <<-EOT
    Name for the storage zone and the pull zone. Globally unique across all of
    Bunny, and a deleted name stays reserved for hours, so if an apply ever
    gets stuck on a name collision, move to the next free -N and re-apply (the
    same thing datadir-datum-site-5 ran into).
  EOT
  type        = string
  default     = "soulaudio-player-site"
}

variable "hostname" {
  description = "Public hostname the pull zone answers on."
  type        = string
  default     = "player.soulaudio.co"
}

variable "storage_region" {
  description = "Bunny Storage main region. DE is Falkenstein, which keeps the origin in the EU."
  type        = string
  default     = "DE"
}
