# Pinned toolchain; Dependabot bumps this tag (see .github/dependabot.yml)
# so base-image updates arrive as PRs that CI builds and eval-tests.
FROM rust:1.99-alpine AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
# The musl target is statically linked by default: the runtime image needs
# nothing but the binary.
RUN cargo build --release

# Shell-free runtime: no package manager, no curl, no apt surface.
FROM scratch
COPY --from=builder /src/target/release/rustysqueezer /rustysqueezer
EXPOSE 8000
USER 65534:65534
ENTRYPOINT ["/rustysqueezer"]
