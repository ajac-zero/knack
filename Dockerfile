FROM rust:1-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY knack-core ./knack-core
COPY knack-registry ./knack-registry
COPY knack-cli ./knack-cli

RUN cargo build --release -p knack-registry

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git openssh-client \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/knack-registry /usr/local/bin/knack-registry

WORKDIR /data

EXPOSE 7349

ENTRYPOINT ["knack-registry"]
CMD ["--index", "/data/knack.index.toml", "--skills-root", "/data/skills", "--public-alias", "company", "--bind", "0.0.0.0:7349"]
