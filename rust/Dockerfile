FROM rust:latest AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/local-compact /usr/local/bin/local-compact
EXPOSE 8000
CMD ["local-compact"]
