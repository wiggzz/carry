output "artifact_bucket_name" {
  description = "Private bucket used for benchmark input manifests and compact results."
  value       = aws_s3_bucket.artifacts.bucket
}

output "github_dispatch_role_arn" {
  description = "Role for the protected GitHub Environment to launch and terminate tagged ephemeral workers."
  value       = aws_iam_role.github_dispatch.arn
}

output "worker_launch_template_id" {
  description = "Immutable launch-template ID used by the later manual benchmark workflow."
  value       = aws_launch_template.worker.id
}

output "artifact_session_role_arn" {
  description = "Run-scoped S3 role; dispatch assumes it with exactly one RunId session tag."
  value       = aws_iam_role.artifact_session.arn
}

output "worker_watchdog_lambda_arn" {
  description = "Lambda invoked every five minutes to enforce the EC2 LaunchTime runtime ceiling."
  value       = aws_lambda_function.worker_watchdog.arn
}

output "worker_security_group_id" {
  description = "Security group with no inbound access and only HTTPS/DNS egress."
  value       = aws_security_group.worker.id
}

output "worker_instance_profile_name" {
  description = "Zero-permission instance profile; artifacts use short-lived pre-signed URLs instead of worker AWS credentials."
  value       = aws_iam_instance_profile.worker.name
}
