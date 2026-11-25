terraform {
  required_version = ">= 1.11"

  required_providers {
    bunnynet = {
      source  = "BunnyWay/bunnynet"
      version = "~> 0.18"
    }
  }
  backend "s3" {}
}
