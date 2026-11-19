# Multi LLM server - multi-stage build (Rust -> distroless-style Debian slim).
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY server ./server
COPY cli ./cli
RUN cargo build --release -p multillm-server

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data multillm \
 && mkdir -p /data && chown multillm /data
COPY --from=build /src/target/release/multillm-server /usr/local/bin/multillm-server
ENV MULTI_LLM_DATA_DIR=/data \
    MULTI_LLM_PORT=5000 \
    MULTI_LLM_SEARXNG_URL=http://searxng:8080
USER multillm
VOLUME ["/data"]
EXPOSE 5000
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
  CMD curl -fsS http://127.0.0.1:5000/api/server-info >/dev/null || exit 1
ENTRYPOINT ["/usr/bin/tini", "--", "multillm-server"]
