# syntax=docker/dockerfile:1.7

# ---------- Stage 1: Rust builder ----------
FROM rust:1.82-slim-bookworm AS rust-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml ./
COPY crates ./crates
RUN cargo build --release -p ash-cli
RUN cargo test --release --workspace --no-run

# ---------- Stage 2: Python builder (uv) ----------
FROM python:3.12-slim-bookworm AS py-builder
COPY --from=ghcr.io/astral-sh/uv:0.5.11 /uv /usr/local/bin/uv
WORKDIR /opt/ashpy
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
    && rm -rf /var/lib/apt/lists/*
ENV UV_PROJECT_ENVIRONMENT=/opt/ashpy/venv \
    UV_LINK_MODE=copy \
    UV_COMPILE_BYTECODE=1
COPY ashpy/pyproject.toml ashpy/uv.lock* ./
COPY ashpy/src ./src
COPY ashpy/tests ./tests
RUN uv sync --frozen --extra dev

# ---------- Stage 3: Runtime ----------
# Uses the same python:3.12 base as the py-builder so that the copied
# virtualenv's interpreter symlink resolves correctly.
FROM python:3.12-slim-bookworm AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates git ripgrep supervisor \
    && rm -rf /var/lib/apt/lists/*

# Rust binary
COPY --from=rust-builder /build/target/release/ash /usr/local/bin/ash

# Python sidecar (copy venv from builder; same debian base so ABI matches)
COPY --from=py-builder /opt/ashpy /opt/ashpy
ENV PATH="/opt/ashpy/venv/bin:${PATH}"

# Supervisor config
COPY docker/supervisord.conf /etc/supervisor/conf.d/ash.conf
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

WORKDIR /workspace
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["supervisord"]
