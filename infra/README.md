# Carry SWE-bench worker infrastructure

This directory defines the **idle-free AWS foundation** for manually dispatched,
canonical SWE-bench Verified runs. It creates no running EC2 instance during
`terraform apply`.

## What it creates

- a private, SSE-S3-encrypted artifact bucket with public access blocked, TLS
  enforcement, and automatic expiry;
- a no-inbound worker security group: HTTPS egress for the parent model client
  and image/dependency downloads, plus VPC DNS only;
- a zero-permission EC2 instance role and instance profile;
- an immutable EC2 launch template with IMDSv2 required, encrypted 150-GiB gp3
  root disk, and terminate-on-shutdown/delete-on-termination behavior;
- a GitHub Actions OIDC dispatch role restricted to the protected
  `wiggzz/carry` `swe-bench` Environment. It can launch/tag/observe/terminate
  only designated benchmark workers;
- a session-tagged artifact role: the dispatch job must assume it with one
  `RunId`, which restricts S3 read/write/list operations to `runs/<RunId>/*`;
- a periodic EventBridge → Lambda watchdog. It independently verifies canonical
  tags and terminates workers that exceed the fixed LaunchTime runtime ceiling.
  It is serverless only (no running EC2 or persistent runner) and its logs expire.

The later workflow will use short-lived **pre-signed** URLs for the worker
manifest and result uploads. The worker instance role intentionally has no AWS
permissions, so agent-created processes cannot obtain useful AWS credentials.

## Deliberate boundaries

- This is one flat configuration: no dev/prod environments.
- It does **not** create a VPC, NAT gateway, public ingress rule, SSH key, or a
  permanent self-hosted runner. A NAT gateway would create an avoidable idle
  charge; use an existing public subnet with no inbound security-group rules.
- It does **not** create a model secret. Add `BENCHMARK_OPENAI_API_KEY` later to
  the protected GitHub Environment only. Never put model credentials in EC2
  user data, S3 objects, Terraform variables, state, Docker environments, or
  task workspaces.
- The AMI is a required explicit input, not a moving “latest” lookup. Its build
  must pin Docker, Bubblewrap, the stable SWE-bench harness, and runner
  dependencies before a live benchmark is enabled.

## Prerequisites

1. An existing GitHub Actions OIDC provider for
   `token.actions.githubusercontent.com` in the target AWS account. This
   account already appears to use that pattern elsewhere; it is referenced, not
   recreated, to avoid a duplicate global IAM provider.
2. An existing **public** subnet with an internet route. The security group
   has no inbound rules; the public address is solely for outbound TLS through
   the internet gateway.
3. A remote Terraform-state backend chosen and configured by the operator.
   Do not commit a local `.tfstate` or credentials. This configuration does not
   bootstrap a state bucket because Terraform cannot safely use a bucket it is
   simultaneously creating as its own backend.
4. Apply credentials with IAM/VPC/EC2/S3 permission. The currently available
   local automation identity is intentionally too restricted to inspect or
   create IAM OIDC resources, so apply with the account’s infrastructure role.

## Validate and plan

Copy the example locally, fill the account-specific values, then use a remote
backend before applying:

1. `cp terraform.tfvars.example terraform.tfvars`
2. `terraform init -backend-config=...`
3. `terraform fmt -check -recursive`
4. `terraform validate`
5. `terraform plan -out=tfplan`

Review that the plan creates a bucket, four narrowly scoped roles, one instance
profile, one security group, one launch template, one Lambda, and one periodic
EventBridge rule—**not** an `aws_instance` or a persistent runner. Apply only
from a trusted operator environment.

## Operator apply steps

The scripts are deliberately manual: none uses `-auto-approve`, and `apply.sh`
requires typing `APPLY` after rendering the saved plan.

1. Create a private local `infra/backend.hcl` from `backend.hcl.example`, using a
   **pre-existing, separate** Terraform-state bucket. Do not use the benchmark
   artifact bucket for state.
2. Create `infra/terraform.tfvars` from `terraform.tfvars.example`; supply a
   pinned AMI ID, a public-subnet ID, the existing GitHub OIDC-provider ARN, and
   a globally unique artifact-bucket name. No model credential belongs here.
3. From the repository root, run `infra/scripts/preflight.sh`.
4. Initialize the chosen remote state: `infra/scripts/init.sh infra/backend.hcl`.
5. Create and inspect the saved plan: `infra/scripts/plan.sh`.
6. Apply that exact reviewed plan: `infra/scripts/apply.sh`.

If apply fails, retain the terminal output and the saved plan if it is still
valid; correct the Terraform/configuration issue and rerun preflight → plan →
apply. Never retry with a hand-edited state file or `-auto-approve`.

Every taggable resource receives `Application=Carry`,
`Repository=wiggzz/carry`, and `Component=swebench-benchmark`. Workers and
volumes additionally have the canonical lifecycle tags used by IAM/watchdog
checks: `ManagedBy=carry-swebench`, `Project=carry-swebench`, and
`Purpose=benchmark-worker`.

## Runtime contract for the later workflow

The protected dispatch job assumes `github_dispatch_role_arn` with GitHub OIDC,
then assumes `artifact_session_role_arn` with exactly one `RunId` session tag
before it writes `runs/<RunId>/...` or signs worker URLs. It launches the
Terraform-managed launch template with the canonical `ManagedBy`, `Project`, and
`Purpose` tags plus a UTC `ExpiresAt` tag. The independent watchdog runs every
five minutes and terminates matching workers no later than
`worker_max_runtime_minutes` after EC2 `LaunchTime`, regardless of a malformed
or future-dated `ExpiresAt` value.

The worker must also self-shutdown on success/failure; the GitHub job has a
final tagged-instance termination step for cancellation/timeout. Those are
defense in depth—the independent watchdog prevents a broken worker or cancelled
workflow from leaving compute running. Results must be strictly validated before
a separate GitHub reporting job posts any PR comment.
