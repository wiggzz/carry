terraform {
  required_version = ">= 1.6.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}

provider "aws" {
  region = var.aws_region

  default_tags {
    tags = {
      Application = "Carry"
      Component   = "swebench-benchmark"
      Repository  = "wiggzz/carry"
    }
  }
}

# ECR Public control-plane APIs are served from us-east-1. Images remain
# anonymously pullable by disposable workers in any AWS Region.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"

  default_tags {
    tags = {
      Application = "Carry"
      Component   = "swebench-benchmark"
      Repository  = "wiggzz/carry"
    }
  }
}
