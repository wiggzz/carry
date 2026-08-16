#!/usr/bin/env python3
"""Generate narrowly scoped S3 GET/PUT pre-signed URLs."""
from __future__ import annotations

import argparse


def generate(client, *, bucket: str, key: str, method: str, expires: int) -> str:
    if method not in {"get", "put"}:
        raise ValueError("method must be get or put")
    if not bucket or not key or expires < 1 or expires > 21600:
        raise ValueError("invalid pre-sign parameters")
    operation = "get_object" if method == "get" else "put_object"
    return client.generate_presigned_url(
        operation,
        Params={"Bucket": bucket, "Key": key},
        ExpiresIn=expires,
        HttpMethod=method.upper(),
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("method", choices=("get", "put"))
    parser.add_argument("bucket")
    parser.add_argument("key")
    parser.add_argument("--expires", type=int, required=True)
    args = parser.parse_args()
    import boto3

    print(generate(
        boto3.client("s3"), bucket=args.bucket, key=args.key,
        method=args.method, expires=args.expires,
    ))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
