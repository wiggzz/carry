#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
INFRA_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)

AWS_REGION=${AWS_REGION:-$(aws configure get region 2>/dev/null || true)}
AWS_REGION=${AWS_REGION:-us-west-2}
STAGE=${STAGE:-swebench}
GITHUB_ENVIRONMENT="swe-bench"

[[ "$STAGE" =~ ^([a-z0-9]|[a-z0-9][a-z0-9-]{0,10}[a-z0-9])$ ]] || {
  echo "STAGE must be 1-12 lowercase letters, digits, or hyphens" >&2
  exit 64
}

export AWS_DEFAULT_REGION="$AWS_REGION"
ACCOUNT_ID=$(aws sts get-caller-identity --query Account --output text)
[[ "$ACCOUNT_ID" =~ ^[0-9]{12}$ ]] || { echo "could not determine AWS account" >&2; exit 69; }

EXPECTED_STATE_BUCKET="carry-tfstate-${ACCOUNT_ID}-${AWS_REGION}-${STAGE}"
if [[ -n "${TF_STATE_BUCKET:-}" && "$TF_STATE_BUCKET" != "$EXPECTED_STATE_BUCKET" ]]; then
  echo "TF_STATE_BUCKET must match the deterministic benchmark state bucket: $EXPECTED_STATE_BUCKET" >&2
  exit 64
fi
export TF_VAR_stage="$STAGE"
STATE_BUCKET="$EXPECTED_STATE_BUCKET"
ARTIFACT_BUCKET=${ARTIFACT_BUCKET:-"carry-artifacts-${ACCOUNT_ID}-${AWS_REGION}-${STAGE}"}
STATE_KEY="carry/swebench-benchmark-infra/${STAGE}.tfstate"
MANIFEST_KEY="carry/swebench-benchmark-infra/${STAGE}.deployment.json"
OIDC_PROVIDER_ARN="arn:aws:iam::${ACCOUNT_ID}:oidc-provider/token.actions.githubusercontent.com"
BACKEND_CONFIG="$INFRA_DIR/backend.hcl"
TFVARS="$INFRA_DIR/terraform.tfvars"

create_state_bucket() {
  if ! aws s3api head-bucket --bucket "$STATE_BUCKET" 2>/dev/null; then
    if [[ "$AWS_REGION" == "us-east-1" ]]; then
      aws s3api create-bucket --bucket "$STATE_BUCKET" --region "$AWS_REGION" >/dev/null
    else
      aws s3api create-bucket --bucket "$STATE_BUCKET" --region "$AWS_REGION" \
        --create-bucket-configuration "LocationConstraint=$AWS_REGION" >/dev/null
    fi
  fi

  aws s3api put-bucket-versioning --bucket "$STATE_BUCKET" --versioning-configuration Status=Enabled
  aws s3api put-bucket-encryption --bucket "$STATE_BUCKET" --server-side-encryption-configuration '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}'
  aws s3api put-public-access-block --bucket "$STATE_BUCKET" --public-access-block-configuration \
    BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
  aws s3api put-bucket-tagging --bucket "$STATE_BUCKET" --tagging \
    '{"TagSet":[{"Key":"Application","Value":"Carry"},{"Key":"Component","Value":"swebench-benchmark"},{"Key":"Repository","Value":"wiggzz/carry"},{"Key":"ManagedBy","Value":"apply.sh"}]}'
}

resolve_subnet() {
  if [[ -n "${WORKER_SUBNET_ID:-}" ]]; then
    printf '%s\n' "$WORKER_SUBNET_ID"
    return
  fi

  local default_vpc
  default_vpc=$(aws ec2 describe-vpcs --filters Name=isDefault,Values=true --query 'Vpcs[0].VpcId' --output text)
  [[ "$default_vpc" != "None" && -n "$default_vpc" ]] || {
    echo "no default VPC found; set WORKER_SUBNET_ID" >&2
    exit 69
  }
  aws ec2 describe-subnets \
    --filters "Name=vpc-id,Values=$default_vpc" "Name=map-public-ip-on-launch,Values=true" \
    --query 'sort_by(Subnets,&AvailabilityZone)[0].SubnetId' --output text
}

resolve_ami() {
  if [[ -n "${WORKER_AMI_ID:-}" ]]; then
    printf '%s\n' "$WORKER_AMI_ID"
    return
  fi
  aws ssm get-parameter \
    --name /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 \
    --query 'Parameter.Value' --output text
}

aws iam get-open-id-connect-provider --open-id-connect-provider-arn "$OIDC_PROVIDER_ARN" >/dev/null

if [[ ! -f "$TFVARS" ]]; then
  WORKER_SUBNET_ID=$(resolve_subnet)
  WORKER_AMI_ID=$(resolve_ami)
  ROOT_DEVICE_NAME=$(aws ec2 describe-images --image-ids "$WORKER_AMI_ID" \
    --query 'Images[0].BlockDeviceMappings[?Ebs!=`null`].DeviceName | [0]' --output text)
  [[ "$WORKER_SUBNET_ID" != "None" && -n "$WORKER_SUBNET_ID" ]] || { echo "could not resolve a public subnet" >&2; exit 69; }
  [[ "$WORKER_AMI_ID" =~ ^ami-[0-9a-f]+$ ]] || { echo "could not resolve an x86_64 Amazon Linux AMI" >&2; exit 69; }
  [[ "$ROOT_DEVICE_NAME" == /dev/* ]] || { echo "could not resolve the AMI root device" >&2; exit 69; }

  cat > "$TFVARS" <<EOF
aws_region                = "$AWS_REGION"
artifact_bucket_name      = "$ARTIFACT_BUCKET"
github_oidc_provider_arn  = "$OIDC_PROVIDER_ARN"
worker_ami_id             = "$WORKER_AMI_ID"
worker_subnet_id          = "$WORKER_SUBNET_ID"
root_device_name          = "$ROOT_DEVICE_NAME"
EOF
fi

create_state_bucket

cat > "$BACKEND_CONFIG" <<EOF
bucket       = "$STATE_BUCKET"
key          = "$STATE_KEY"
region       = "$AWS_REGION"
encrypt      = true
use_lockfile = true
EOF

if [[ -f "$INFRA_DIR/terraform.tfstate" ]]; then
  terraform -chdir="$INFRA_DIR" init -migrate-state -force-copy -input=false -backend-config="$BACKEND_CONFIG"
else
  terraform -chdir="$INFRA_DIR" init -reconfigure -input=false -backend-config="$BACKEND_CONFIG"
fi
terraform -chdir="$INFRA_DIR" apply -input=false -auto-approve \
  -var "aws_region=$AWS_REGION" \
  -var "stage=$STAGE" \
  -var "github_environment=$GITHUB_ENVIRONMENT"

terraform_outputs=$(mktemp)
deployment_manifest=$(mktemp)
cleanup_manifest_files() { rm -f "$terraform_outputs" "$deployment_manifest"; }
trap cleanup_manifest_files EXIT
terraform -chdir="$INFRA_DIR" output -json > "$terraform_outputs"
python3 "$INFRA_DIR/../scripts/benchmark_deployment_manifest.py" write \
  --terraform-output "$terraform_outputs" \
  --backend-bucket "$STATE_BUCKET" \
  --backend-key "$STATE_KEY" \
  --backend-region "$AWS_REGION" \
  --output "$deployment_manifest"
aws s3 cp "$deployment_manifest" "s3://$STATE_BUCKET/$MANIFEST_KEY" --sse AES256 --only-show-errors
printf 'Published non-secret benchmark deployment manifest: s3://%s/%s\n' "$STATE_BUCKET" "$MANIFEST_KEY"
