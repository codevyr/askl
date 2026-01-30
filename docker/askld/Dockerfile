# ---- Build stage ----
ARG RUST_VERSION=1.89
FROM rust:${RUST_VERSION}-trixie AS builder

# System deps for building with sqlite
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libsqlite3-dev \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Copy the full workspace (simple & robust; rely on Docker layer cache)
COPY . .

ENV DATABASE_URL=sqlite:./test.db?mode=rwc
RUN rm -f test.db && cargo run -p create-index -- -o test.db

# Build just the askld crate, with lockfile respected
RUN cargo build --locked --release -p askld

# ---- Runtime stage ----
FROM debian:trixie-slim AS runtime

# Minimal runtime deps (certs, tzdata, sqlite runtime/cli)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates tzdata sqlite3 libsqlite3-0 libpq5 netcat-openbsd \
 && rm -rf /var/lib/apt/lists/*

# Non-root user with uid 1000 to match your flake config
RUN useradd -u 1000 -m appuser

# Copy the compiled binary from the builder
COPY --from=builder /src/target/release/askld /usr/local/bin/askld

# Wait for Postgres when configured, then exec askld.
COPY docker/askld/askld-entrypoint.sh /usr/local/bin/askld-entrypoint
RUN chmod +x /usr/local/bin/askld-entrypoint

WORKDIR /app
VOLUME ["/data"]
EXPOSE 80
USER 1000

# Same args you used in the flake.
ENTRYPOINT ["/usr/local/bin/askld-entrypoint"]
CMD ["serve","--index","/data/index.db","--format","sqlite","--host","0.0.0.0"]
