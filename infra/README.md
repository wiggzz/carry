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
  `RunId`, which restricts S3 read/write/list operations to `runs/<RunId>/*`.

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

## One-command deploy

The Terraform state bucket must exist **once** before Terraform can initialize its
S3 backend. It is deliberately separate from the benchmark artifact bucket. This
is the only AWS resource to create outside this configuration; Terraform cannot
create the bucket it needs as its own backend.

1. Create one private S3 state bucket in the target account, with versioning and
   encryption enabled. Do not use the benchmark artifact bucket.
2. Copy `backend.hcl.example` to ignored `backend.hcl` and set that state bucket.
3. Copy `terraform.tfvars.example` to ignored `terraform.tfvars` and set the
   pinned AMI, existing public subnet, GitHub OIDC provider ARN, and artifact
   bucket name.
4. Run the only deployment command from the repository root:

   ```sh
   infra/scripts/apply.sh
   ```

The script initializes the configured backend and applies non-interactively. It
never reads credentials or model keys from files in this repository.

Every taggable resource receives `Application=Carry`,
`Repository=wiggzz/carry`, and `Component=swebench-benchmark`. Workers and
volumes additionally have the canonical lifecycle tags used by IAM cleanup
checks: `ManagedBy=carry-swebench`, `Project=carry-swebench`,
`Purpose=benchmark-worker`, and a per-dispatch `RunId` (`gh-*`). The `RunId`
correlates one manual workflow run for narrow recovery; exact instance-ID cleanup
remains the normal path.

## Runtime contract for the later workflow

The protected dispatch job assumes `github_dispatch_role_arn` with GitHub OIDC,
then assumes `artifact_session_role_arn` with exactly one `RunId` session tag
before it writes `runs/<RunId>/...` or signs worker URLs. It launches the
Terraform-managed launch template with the canonical `ManagedBy`, `Project`, and
`Purpose` tags plus a UTC `ExpiresAt` tag.

Workers use EC2 instance-initiated shutdown with `terminate`, and their root
volumes delete on termination. For the initial manually observed runs, there is
intentionally no persistent cloud watchdog. The later protected dispatch workflow
must have an `if: always()` final step that terminates its exact tagged worker
instance by ID after success, failure, cancellation, or job timeout. Results must
be strictly validated before a separate GitHub reporting job posts any PR comment.
