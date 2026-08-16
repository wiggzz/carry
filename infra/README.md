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
- It does **not** create or store a model secret. The protected GitHub Environment
  supplies `OPENAI_API_KEY` only for an explicitly selected smoke/live dispatch.
  That workflow uploads it as an SSE-encrypted, automatically expiring run object
  and gives the zero-permission worker a short-lived pre-signed GET. The worker
  keeps it in root-only tmpfs, forwards it by environment name only to disposable
  agent containers, deletes it before evaluation, and removes the run object in
  controller cleanup. It never enters EC2 user data, Terraform state, task input,
  evaluator configuration, or result metadata.
- The AMI is a required explicit input, not a moving “latest” lookup. The initial
  smoke path installs Docker and pinned harness dependencies at boot and records
  run-local image identities; a prebuilt image pipeline can replace that later
  without changing the benchmark contract.

## Prerequisites

1. An existing GitHub Actions OIDC provider for
   `token.actions.githubusercontent.com` in the target account. The deploy script
   derives and verifies its ARN; it does not create a duplicate global provider.
   The dispatch role is bound to this repository's immutable GitHub OIDC subject
   prefix and the `swe-bench` Environment; update that input if the repository is
   transferred or recreated.
2. AWS credentials allowed to create/read S3, VPC/EC2, IAM, and the Terraform
   resources. No model credential is used.
3. A default VPC with a public subnet, or `WORKER_SUBNET_ID` set for an existing
   public subnet with outbound internet access. The worker security group has no
   inbound rules.

## One-command deploy

From the repository root:

```sh
infra/scripts/apply.sh
```

With no configuration files or prompts, the script derives the account and OIDC
provider, creates a separate private/versioned/encrypted Terraform-state bucket,
calculates an artifact-bucket name, selects a public subnet from the default VPC,
and resolves a concrete Amazon Linux x86_64 AMI/root device. It writes the ignored
local `backend.hcl` and `terraform.tfvars`, then initializes Terraform and applies
it non-interactively. The generated values stay pinned in `terraform.tfvars` for
later applies.

Optional first-run overrides:

```sh
AWS_REGION=us-west-2 STAGE=swebench infra/scripts/apply.sh
WORKER_SUBNET_ID=subnet-... WORKER_AMI_ID=ami-... infra/scripts/apply.sh
```

`AWS_REGION` otherwise comes from the configured AWS CLI profile (then defaults to
`us-west-2`); `STAGE` defaults to `swebench`. The automatic Amazon Linux AMI is
sufficient for the credential-free bootstrap canary. Set `WORKER_AMI_ID` to a
reviewed worker image before enabling the future external-container live runner.

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
