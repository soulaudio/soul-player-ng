terraform {
  required_version = ">= 1.11"

  required_providers {
    bunnynet = {
      # Exact, not a range: 0.20.3 was published and then withdrawn, and a CI
      # run that had it in the lock failed with "the previously-selected
      # version is no longer available" while a local resolve still offered
      # it. Pinning the version CI has actually run keeps the two honest.
      # Bump deliberately.
      source  = "BunnyWay/bunnynet"
      version = "0.20.2"
    }
    # Only to wait out DNS propagation before Bunny tries to issue the
    # certificate; see the hostname resource.
    time = {
      source  = "hashicorp/time"
      version = "~> 0.12"
    }
  }
  backend "s3" {}
}
