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

output "worker_launch_template_version" {
  description = "Numeric launch-template version to pin in the protected dispatch workflow."
  value       = aws_launch_template.worker.latest_version
}

output "artifact_session_role_arn" {
  description = "Run-scoped S3 role; dispatch assumes it with exactly one RunId session tag."
  value       = aws_iam_role.artifact_session.arn
}

output "worker_security_group_id" {
  description = "Security group with no inbound access and only HTTPS/DNS egress."
  value       = aws_security_group.worker.id
}

output "worker_instance_profile_name" {
  description = "Zero-permission instance profile; artifacts use short-lived pre-signed URLs instead of worker AWS credentials."
  value       = aws_iam_instance_profile.worker.name
}

output "task_image_repository_uri" {
  description = "Public ECR repository used for immutable prepared SWE-bench task images."
  value       = aws_ecrpublic_repository.task_images.repository_uri
}

output "task_image_publisher_role_arn" {
  description = "Role assumed only by protected preparation runs to push task images."
  value       = aws_iam_role.task_image_publisher.arn
}
