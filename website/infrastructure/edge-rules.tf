# Cache and headers at the edge.
#
# The page has two kinds of content with opposite needs. /_astro/* is hashed
# and immutable, so it can be cached for a year. Everything else states the
# current release version, so it must never be stale: index.html, version.json
# (which the scheduled sync job reads to decide whether the site has fallen
# behind), and install.sh.

# Hashed assets never change under the same name.
resource "bunnynet_pullzone_edgerule" "immutable_assets" {
  pullzone    = bunnynet_pullzone.player.id
  description = "Cache /_astro/* immutable assets for 1 year"
  enabled     = true
  match_type  = "MatchAny"

  actions = [
    {
      type       = "OverrideCacheTime"
      parameter1 = "31536000"
      parameter2 = null
      parameter3 = null
    },
    # OverrideCacheTime is the EDGE cache only: the browser still gets the pull
    # zone default, which is 300s, so a visitor re-fetched hashed assets every
    # five minutes. Verified against the live headers, not assumed.
    {
      type       = "SetResponseHeader"
      parameter1 = "Cache-Control"
      parameter2 = "public, max-age=31536000, immutable"
      parameter3 = null
    },
  ]

  triggers = [{
    type       = "Url"
    match_type = "MatchAny"
    patterns   = ["*/_astro/*", "*/fonts/*"]
    parameter1 = null
    parameter2 = null
  }]
}

# The HTML names a version, so it revalidates. Without this it inherits the
# storage origin's ~296-day max-age and a visitor's own browser cache keeps
# showing an old release that no purge can reach.
resource "bunnynet_pullzone_edgerule" "html_short_cache" {
  pullzone    = bunnynet_pullzone.player.id
  description = "Short, revalidating cache for HTML"
  enabled     = true
  match_type  = "MatchAny"

  actions = [{
    type       = "SetResponseHeader"
    parameter1 = "Cache-Control"
    parameter2 = "public, max-age=300, must-revalidate"
    parameter3 = null
  }]

  triggers = [{
    type       = "Url"
    match_type = "MatchAny"
    patterns   = ["*/", "*.html"]
    parameter1 = null
    parameter2 = null
  }]
}

# version.json is the sync check's source of truth: the scheduled job compares
# it against the latest release. A cached copy would make a stale site look
# current and skip the redeploy that fixes it.
resource "bunnynet_pullzone_edgerule" "version_no_store" {
  pullzone    = bunnynet_pullzone.player.id
  description = "Never cache version.json, the deploy checks it"
  enabled     = true
  match_type  = "MatchAny"

  actions = [{
    type       = "SetResponseHeader"
    parameter1 = "Cache-Control"
    parameter2 = "no-store"
    parameter3 = null
  }]

  triggers = [{
    type       = "Url"
    match_type = "MatchAny"
    patterns   = ["*/version.json*"]
    parameter1 = null
    parameter2 = null
  }]
}

# People read install.sh in a browser before piping it to a shell, which only
# works if it is served as text rather than as a download. It also points at
# the current release, so it revalidates like the HTML.
resource "bunnynet_pullzone_edgerule" "installer_as_text" {
  pullzone    = bunnynet_pullzone.player.id
  description = "Serve install.sh as readable text, cached briefly"
  enabled     = true
  match_type  = "MatchAny"

  actions = [
    {
      type       = "SetResponseHeader"
      parameter1 = "Content-Type"
      parameter2 = "text/plain; charset=utf-8"
      parameter3 = null
    },
    {
      type       = "SetResponseHeader"
      parameter1 = "Cache-Control"
      parameter2 = "public, max-age=300, must-revalidate"
      parameter3 = null
    },
  ]

  triggers = [{
    type       = "Url"
    match_type = "MatchAny"
    patterns   = ["*/install.sh"]
    parameter1 = null
    parameter2 = null
  }]
}

# The page is static, same-origin throughout, and loads nothing from anywhere
# else, so the policy can be as tight as it reads. If a third-party anything
# ever lands on the page, this is the thing that will break first, on purpose.
resource "bunnynet_pullzone_edgerule" "security_headers" {
  pullzone    = bunnynet_pullzone.player.id
  description = "HSTS, CSP, and the usual hardening headers"
  enabled     = true
  match_type  = "MatchAny"

  actions = [
    {
      type       = "SetResponseHeader"
      parameter1 = "Strict-Transport-Security"
      parameter2 = "max-age=31536000; includeSubDomains"
      parameter3 = null
    },
    {
      type       = "SetResponseHeader"
      parameter1 = "Content-Security-Policy"
      parameter2 = "default-src 'none'; img-src 'self'; style-src 'self'; script-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
      parameter3 = null
    },
    {
      type       = "SetResponseHeader"
      parameter1 = "X-Content-Type-Options"
      parameter2 = "nosniff"
      parameter3 = null
    },
    {
      type       = "SetResponseHeader"
      parameter1 = "Referrer-Policy"
      parameter2 = "strict-origin-when-cross-origin"
      parameter3 = null
    },
  ]

  triggers = [{
    type       = "Url"
    match_type = "MatchAny"
    patterns   = ["*"]
    parameter1 = null
    parameter2 = null
  }]
}
