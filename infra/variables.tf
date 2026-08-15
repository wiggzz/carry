variable "aws_region" {
  description = "AWS region containing the worker subnet and artifact bucket."
  type        = string
  default     = "us-west-2"
}

variable "artifact_bucket_name" {
  description = "Globally unique private S3 bucket name for benchmark manifests and result artifacts."
  type        = string

  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$", var.artifact_bucket_name))
    error_message = "artifact_bucket_name must be a valid 3-63 character lowercase S3 bucket name."
  }
}

variable "github_oidc_provider_arn" {
  description = "Existing IAM OIDC provider ARN for token.actions.githubusercontent.com. This configuration deliberately does not create a duplicate provider."
  type        = string
}

variable "github_repository" {
  description = "GitHub repository allowed to assume the dispatch role."
  type        = string
  default     = "wiggzz/carry"

  validation {
    condition     = can(regex("^[^/ ]+/[^/ ]+$", var.github_repository))
    error_message = "github_repository must be in owner/repository form."
  }
}

variable "github_environment" {
  description = "Protected GitHub Environment required to assume the dispatch role."
  type        = string
  default     = "swe-bench"
}

variable "watchdog_log_retention_days" {
  description = "CloudWatch retention for one-shot worker watchdog invocations."
  type        = number
  default     = 14

  validation {
    condition     = contains([1, 3, 5, 7, 14, 30, 60, 90], var.watchdog_log_retention_days)
    error_message = "watchdog_log_retention_days must be a supported CloudWatch retention value."
  }
}

variable "worker_ami_id" {
  description = "Pinned x86_64 worker AMI ID. Build and review this image separately; never use a moving latest-AMI lookup for benchmark runs."
  type        = string
}

variable "worker_subnet_id" {
  description = "Public subnet ID for ephemeral workers. It must have an Internet route; the worker security group has no inbound rules."
  type        = string
}

variable "worker_instance_type" {
  description = "x86_64 instance type used by the immutable worker launch template."
  type        = string
  default     = "m7i.2xlarge"
}

variable "worker_max_runtime_minutes" {
  description = "Hard upper bound enforced by the independent watchdog from EC2 LaunchTime."
  type        = number
  default     = 720

  validation {
    condition     = var.worker_max_runtime_minutes >= 30 && var.worker_max_runtime_minutes <= 1440
    error_message = "worker_max_runtime_minutes must be between 30 minutes and 24 hours."
  }
}

variable "worker_root_volume_gib" {
  description = "Encrypted gp3 root-volume size. Official SWE-bench Docker grading requires at least 120 GiB."
  type        = number
  default     = 150

  validation {
    condition     = var.worker_root_volume_gib >= 120
    error_message = "worker_root_volume_gib must be at least 120 GiB for official SWE-bench evaluation images."
  }
}

variable "root_device_name" {
  description = "Root block-device path expected by the pinned worker AMI."
  type        = string
  default     = "/dev/sda1"
}

variable "artifact_retention_days" {
  description = "Days to retain run artifacts before automatic deletion."
  type        = number
  default     = 14

  validation {
    condition     = var.artifact_retention_days >= 1 && var.artifact_retention_days <= 365
    error_message = "artifact_retention_days must be between 1 and 365."
  }
}
