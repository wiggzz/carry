data "aws_partition" "current" {}

data "aws_caller_identity" "current" {}

data "aws_subnet" "worker" {
  id = var.worker_subnet_id
}

data "aws_vpc" "worker" {
  id = data.aws_subnet.worker.vpc_id
}

locals {
  name_prefix = "carry-swebench"
  common_tags = {
    Application = "Carry"
    Component   = "swebench-benchmark"
    ManagedBy   = "terraform"
    Project     = "carry-swebench"
    Repository  = "wiggzz/carry"
  }
  worker_resource_tags = {
    Application = "Carry"
    Component   = "swebench-benchmark"
    ManagedBy   = "carry-swebench"
    Project     = "carry-swebench"
    Purpose     = "benchmark-worker"
    Repository  = "wiggzz/carry"
  }
}

resource "aws_ecrpublic_repository" "task_images" {
  provider        = aws.us_east_1
  repository_name = "carry-swebench-tasks"

  catalog_data {
    description = "Public, sanitized, readiness-checked SWE-bench task images"
  }

  tags = merge(local.common_tags, {
    Name = "${local.name_prefix}-tasks"
  })
}

resource "aws_s3_bucket" "artifacts" {
  bucket        = var.artifact_bucket_name
  force_destroy = false

  tags = merge(local.common_tags, {
    Name = "${local.name_prefix}-artifacts"
  })
}

resource "aws_s3_bucket_ownership_controls" "artifacts" {
  bucket = aws_s3_bucket.artifacts.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_public_access_block" "artifacts" {
  bucket = aws_s3_bucket.artifacts.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "artifacts" {
  bucket = aws_s3_bucket.artifacts.id

  rule {
    bucket_key_enabled = false

    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "artifacts" {
  bucket = aws_s3_bucket.artifacts.id

  rule {
    id     = "expire-run-artifacts"
    status = "Enabled"

    filter {}

    expiration {
      days = var.artifact_retention_days
    }

    abort_incomplete_multipart_upload {
      days_after_initiation = 1
    }
  }
}

data "aws_iam_policy_document" "artifact_bucket" {
  statement {
    sid    = "DenyInsecureTransport"
    effect = "Deny"

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    actions = ["s3:*"]

    resources = [
      aws_s3_bucket.artifacts.arn,
      "${aws_s3_bucket.artifacts.arn}/*",
    ]

    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }
}

resource "aws_s3_bucket_policy" "artifacts" {
  bucket = aws_s3_bucket.artifacts.id
  policy = data.aws_iam_policy_document.artifact_bucket.json
}

resource "aws_security_group" "worker" {
  name_prefix = "${local.name_prefix}-worker-"
  description = "No-inbound ephemeral SWE-bench worker security group"
  vpc_id      = data.aws_vpc.worker.id

  # The parent agent needs TLS for model calls and image/dependency downloads.
  # Agent-created shell commands run in the separate no-network Bubblewrap sandbox.
  egress {
    description = "HTTPS for model API and public benchmark dependencies"
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    description = "DNS UDP to the selected VPC resolver"
    from_port   = 53
    to_port     = 53
    protocol    = "udp"
    cidr_blocks = [data.aws_vpc.worker.cidr_block]
  }

  egress {
    description = "DNS TCP to the selected VPC resolver"
    from_port   = 53
    to_port     = 53
    protocol    = "tcp"
    cidr_blocks = [data.aws_vpc.worker.cidr_block]
  }

  tags = merge(local.common_tags, {
    Name = "${local.name_prefix}-worker"
  })
}

data "aws_iam_policy_document" "worker_assume" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

# This role intentionally has no permissions. The runner receives only scoped,
# short-lived pre-signed URLs for manifests and result uploads.
resource "aws_iam_role" "worker" {
  name_prefix        = "${local.name_prefix}-worker-"
  assume_role_policy = data.aws_iam_policy_document.worker_assume.json

  tags = local.common_tags
}

resource "aws_iam_instance_profile" "worker" {
  name_prefix = "${local.name_prefix}-worker-"
  role        = aws_iam_role.worker.name

  tags = local.common_tags
}

data "aws_iam_policy_document" "github_dispatch_assume" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [var.github_oidc_provider_arn]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      values   = ["${var.github_oidc_subject_prefix}:environment:${var.github_environment}"]
    }
  }
}

resource "aws_iam_role" "github_dispatch" {
  name_prefix          = "${local.name_prefix}-github-dispatch-"
  assume_role_policy   = data.aws_iam_policy_document.github_dispatch_assume.json
  max_session_duration = 21600

  tags = local.common_tags
}

data "aws_iam_policy_document" "github_dispatch" {
  # RunInstances authorizes every request resource independently. AWS's
  # launch-template policy pattern requires a separate network/subnet statement:
  # IsLaunchTemplateResource is evaluated for those resources but not for every
  # resource that RunInstances touches.
  statement {
    sid     = "LaunchOnlyFromCanonicalTemplate"
    effect  = "Allow"
    actions = ["ec2:RunInstances"]
    not_resources = [
      "arn:${data.aws_partition.current.partition}:ec2:${var.aws_region}:${data.aws_caller_identity.current.account_id}:network-interface/*",
      "arn:${data.aws_partition.current.partition}:ec2:${var.aws_region}:${data.aws_caller_identity.current.account_id}:subnet/*",
    ]

    condition {
      test     = "ArnLike"
      variable = "ec2:LaunchTemplate"
      values   = [aws_launch_template.worker.arn]
    }
  }

  statement {
    sid     = "LaunchNetworkOnlyFromCanonicalTemplate"
    effect  = "Allow"
    actions = ["ec2:RunInstances"]
    resources = [
      "arn:${data.aws_partition.current.partition}:ec2:${var.aws_region}:${data.aws_caller_identity.current.account_id}:network-interface/*",
      "arn:${data.aws_partition.current.partition}:ec2:${var.aws_region}:${data.aws_caller_identity.current.account_id}:subnet/*",
    ]

    condition {
      test     = "ArnLike"
      variable = "ec2:LaunchTemplate"
      values   = [aws_launch_template.worker.arn]
    }

    condition {
      test     = "Bool"
      variable = "ec2:IsLaunchTemplateResource"
      values   = ["true"]
    }
  }

  statement {
    sid       = "TagOnlyAtWorkerCreation"
    effect    = "Allow"
    actions   = ["ec2:CreateTags"]
    resources = ["*"]

    condition {
      test     = "StringEquals"
      variable = "ec2:CreateAction"
      values   = ["RunInstances"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:RequestTag/ManagedBy"
      values   = ["carry-swebench"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:RequestTag/Project"
      values   = ["carry-swebench"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:RequestTag/Purpose"
      values   = ["benchmark-worker"]
    }

    condition {
      test     = "StringLike"
      variable = "aws:RequestTag/ExpiresAt"
      values   = ["????-??-??T??:??:??Z"]
    }

    condition {
      test     = "StringLike"
      variable = "aws:RequestTag/RunId"
      values   = ["gh-*"]
    }

    condition {
      test     = "ForAllValues:StringEquals"
      variable = "aws:TagKeys"
      values   = ["Application", "Component", "ExpiresAt", "ManagedBy", "Project", "Purpose", "Repository", "RunId"]
    }
  }

  statement {
    sid    = "InspectBenchmarkWorkers"
    effect = "Allow"
    actions = [
      "ec2:DescribeInstances",
      "ec2:DescribeInstanceStatus",
      "ec2:DescribeLaunchTemplates",
      "ec2:DescribeLaunchTemplateVersions",
      "ec2:GetConsoleOutput",
    ]
    resources = ["*"]
  }

  statement {
    sid       = "TerminateTaggedBenchmarkWorkers"
    effect    = "Allow"
    actions   = ["ec2:TerminateInstances"]
    resources = ["arn:${data.aws_partition.current.partition}:ec2:${var.aws_region}:${data.aws_caller_identity.current.account_id}:instance/*"]

    condition {
      test     = "StringEquals"
      variable = "ec2:ResourceTag/ManagedBy"
      values   = ["carry-swebench"]
    }

    condition {
      test     = "StringEquals"
      variable = "ec2:ResourceTag/Project"
      values   = ["carry-swebench"]
    }

    condition {
      test     = "StringEquals"
      variable = "ec2:ResourceTag/Purpose"
      values   = ["benchmark-worker"]
    }
  }

  statement {
    sid       = "PassOnlyTheZeroPermissionWorkerProfile"
    effect    = "Allow"
    actions   = ["iam:PassRole"]
    resources = [aws_iam_role.worker.arn]

    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["ec2.amazonaws.com"]
    }
  }

  statement {
    sid    = "AssumeRunScopedArtifactSession"
    effect = "Allow"
    actions = [
      "sts:AssumeRole",
      "sts:TagSession",
    ]
    resources = [aws_iam_role.artifact_session.arn]
  }

  statement {
    sid       = "AssumeTaskImagePublisher"
    effect    = "Allow"
    actions   = ["sts:AssumeRole"]
    resources = [aws_iam_role.task_image_publisher.arn]
  }
}

resource "aws_iam_role_policy" "github_dispatch" {
  name_prefix = "${local.name_prefix}-github-dispatch-"
  role        = aws_iam_role.github_dispatch.id
  policy      = data.aws_iam_policy_document.github_dispatch.json
}

data "aws_iam_policy_document" "task_image_publisher_assume" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRole"]

    principals {
      type        = "AWS"
      identifiers = [aws_iam_role.github_dispatch.arn]
    }
  }
}

resource "aws_iam_role" "task_image_publisher" {
  name_prefix          = "${local.name_prefix}-task-publisher-"
  assume_role_policy   = data.aws_iam_policy_document.task_image_publisher_assume.json
  max_session_duration = 21600

  tags = local.common_tags
}

data "aws_iam_policy_document" "task_image_publisher" {
  statement {
    sid       = "AuthenticateToEcrPublic"
    effect    = "Allow"
    actions   = ["ecr-public:GetAuthorizationToken"]
    resources = ["*"]
  }

  # Amazon ECR Public's documented registry-login prerequisites are
  # ecr-public:GetAuthorizationToken and sts:GetServiceBearerToken. The
  # service-bearer request made by the ECR Public CLI did not satisfy the
  # STS condition key in practice, so keep this narrowly scoped role's
  # required bearer-token permission unconditional.
  statement {
    sid       = "GetEcrPublicBearerToken"
    effect    = "Allow"
    actions   = ["sts:GetServiceBearerToken"]
    resources = ["*"]
  }

  statement {
    sid    = "PublishOnlyPreparedTaskImages"
    effect = "Allow"
    actions = [
      "ecr-public:BatchCheckLayerAvailability",
      "ecr-public:CompleteLayerUpload",
      "ecr-public:DescribeImages",
      "ecr-public:DescribeImageTags",
      "ecr-public:InitiateLayerUpload",
      "ecr-public:PutImage",
      "ecr-public:UploadLayerPart",
    ]
    resources = [aws_ecrpublic_repository.task_images.arn]
  }
}

resource "aws_iam_role_policy" "task_image_publisher" {
  name_prefix = "${local.name_prefix}-task-publisher-"
  role        = aws_iam_role.task_image_publisher.id
  policy      = data.aws_iam_policy_document.task_image_publisher.json
}

data "aws_iam_policy_document" "artifact_session_assume" {
  statement {
    effect = "Allow"
    actions = [
      "sts:AssumeRole",
      "sts:TagSession",
    ]

    principals {
      type        = "AWS"
      identifiers = [aws_iam_role.github_dispatch.arn]
    }

    condition {
      test     = "ForAllValues:StringEquals"
      variable = "sts:TagKeys"
      values   = ["RunId"]
    }

    condition {
      test     = "Null"
      variable = "aws:RequestTag/RunId"
      values   = ["false"]
    }
  }
}

resource "aws_iam_role" "artifact_session" {
  name_prefix        = "${local.name_prefix}-artifact-session-"
  assume_role_policy = data.aws_iam_policy_document.artifact_session_assume.json

  tags = local.common_tags
}

data "aws_iam_policy_document" "artifact_session" {
  statement {
    sid    = "ReadAndWriteOnlyTheTaggedRun"
    effect = "Allow"
    actions = [
      "s3:DeleteObject",
      "s3:GetObject",
      "s3:PutObject",
    ]
    resources = ["${aws_s3_bucket.artifacts.arn}/runs/&{aws:PrincipalTag/RunId}/*"]
  }

  statement {
    sid       = "ListOnlyTheTaggedRun"
    effect    = "Allow"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.artifacts.arn]

    condition {
      test     = "StringLike"
      variable = "s3:prefix"
      values   = ["runs/&{aws:PrincipalTag/RunId}/*"]
    }
  }
}

resource "aws_iam_role_policy" "artifact_session" {
  name_prefix = "${local.name_prefix}-artifact-session-"
  role        = aws_iam_role.artifact_session.id
  policy      = data.aws_iam_policy_document.artifact_session.json
}

resource "aws_launch_template" "worker" {
  name_prefix            = "${local.name_prefix}-worker-"
  description            = "Pinned AMI template for disposable SWE-bench benchmark workers"
  image_id               = var.worker_ami_id
  instance_type          = var.worker_instance_type
  update_default_version = true

  instance_initiated_shutdown_behavior = "terminate"

  iam_instance_profile {
    arn = aws_iam_instance_profile.worker.arn
  }

  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required"
    http_put_response_hop_limit = 1
    instance_metadata_tags      = "disabled"
  }

  network_interfaces {
    associate_public_ip_address = true
    delete_on_termination       = true
    device_index                = 0
    security_groups             = [aws_security_group.worker.id]
    subnet_id                   = var.worker_subnet_id
  }

  block_device_mappings {
    device_name = var.root_device_name

    ebs {
      delete_on_termination = true
      encrypted             = true
      volume_size           = var.worker_root_volume_gib
      volume_type           = "gp3"
    }
  }

  tag_specifications {
    resource_type = "instance"

    tags = local.worker_resource_tags
  }

  tag_specifications {
    resource_type = "volume"

    tags = local.worker_resource_tags
  }

  tags = merge(local.common_tags, {
    Name = "${local.name_prefix}-worker-template"
  })
}
