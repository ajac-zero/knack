FROM rust:1-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY skillhub-core ./skillhub-core
COPY skillhub-registry ./skillhub-registry
COPY skillhub-cli ./skillhub-cli

RUN cargo build --release -p skillhub-registry

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git openssh-client \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/skillhub-registry /usr/local/bin/skillhub-registry

WORKDIR /data

EXPOSE 7349

ENTRYPOINT ["skillhub-registry"]
CMD ["--index", "/data/skillhub.index.toml", "--skills-root", "/data/skills", "--public-alias", "company", "--bind", "0.0.0.0:7349"]
