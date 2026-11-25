terraform {
  required_version = ">= 1.11"

  required_providers {
    bunnynet = {
      source  = "BunnyWay/bunnynet"
      version = "~> 0.18"
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
