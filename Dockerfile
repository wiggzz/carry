FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release

FROM python:3.13-slim-bookworm
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/carry /usr/local/bin/carry
ENTRYPOINT ["carry"]
