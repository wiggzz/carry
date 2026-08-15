data "archive_file" "worker_watchdog" {
  type        = "zip"
  source_file = "${path.module}/lambda/watchdog.py"
  output_path = "${path.module}/.terraform/worker-watchdog.zip"
}

data "aws_iam_policy_document" "worker_watchdog_assume" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "worker_watchdog" {
  name_prefix        = "${local.name_prefix}-watchdog-"
  assume_role_policy = data.aws_iam_policy_document.worker_watchdog_assume.json

  tags = local.common_tags
}

data "aws_iam_policy_document" "worker_watchdog" {
  statement {
    sid       = "DescribeWorkers"
    effect    = "Allow"
    actions   = ["ec2:DescribeInstances"]
    resources = ["*"]
  }

  statement {
    sid       = "TerminateOnlyCanonicalWorkers"
    effect    = "Allow"
    actions   = ["ec2:TerminateInstances"]
    resources = ["arn:${data.aws_partition.current.partition}:ec2:${var.aws_region}:${data.aws_caller_identity.current.account_id}:instance/*"]

    condition {
      test     = "StringEquals"
      variable = "ec2:ResourceTag/ManagedBy"
      values   = [local.worker_resource_tags.ManagedBy]
    }

    condition {
      test     = "StringEquals"
      variable = "ec2:ResourceTag/Project"
      values   = [local.worker_resource_tags.Project]
    }

    condition {
      test     = "StringEquals"
      variable = "ec2:ResourceTag/Purpose"
      values   = [local.worker_resource_tags.Purpose]
    }
  }

  statement {
    sid    = "WriteOwnLogs"
    effect = "Allow"
    actions = [
      "logs:CreateLogStream",
      "logs:PutLogEvents",
    ]
    resources = ["${aws_cloudwatch_log_group.worker_watchdog.arn}:*"]
  }
}

resource "aws_iam_role_policy" "worker_watchdog" {
  name_prefix = "${local.name_prefix}-watchdog-"
  role        = aws_iam_role.worker_watchdog.id
  policy      = data.aws_iam_policy_document.worker_watchdog.json
}

resource "aws_cloudwatch_log_group" "worker_watchdog" {
  name              = "/aws/lambda/${local.name_prefix}-worker-watchdog"
  retention_in_days = var.watchdog_log_retention_days

  tags = local.common_tags
}

resource "aws_lambda_function" "worker_watchdog" {
  function_name = "${local.name_prefix}-worker-watchdog"
  description   = "Terminates benchmark workers beyond the configured maximum runtime"
  role          = aws_iam_role.worker_watchdog.arn
  handler       = "watchdog.lambda_handler"
  runtime       = "python3.12"
  timeout       = 30

  filename         = data.archive_file.worker_watchdog.output_path
  source_code_hash = data.archive_file.worker_watchdog.output_base64sha256

  environment {
    variables = {
      MAX_WORKER_RUNTIME_MINUTES = tostring(var.worker_max_runtime_minutes)
    }
  }

  depends_on = [aws_cloudwatch_log_group.worker_watchdog]

  tags = local.common_tags
}

# This backstop is deliberately periodic and independent of worker/user-data
# behavior. It is serverless only; no EC2 instance or persistent runner is idle.
resource "aws_cloudwatch_event_rule" "worker_watchdog" {
  name                = "${local.name_prefix}-worker-watchdog"
  description         = "Bounded-runtime cleanup for ephemeral Carry benchmark workers"
  schedule_expression = "rate(5 minutes)"

  tags = local.common_tags
}

resource "aws_cloudwatch_event_target" "worker_watchdog" {
  rule = aws_cloudwatch_event_rule.worker_watchdog.name
  arn  = aws_lambda_function.worker_watchdog.arn
}

resource "aws_lambda_permission" "eventbridge_invoke_watchdog" {
  statement_id  = "AllowEventBridgeWatchdogInvocation"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.worker_watchdog.function_name
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.worker_watchdog.arn
}
