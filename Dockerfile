# Multi-stage Rust + React build, with a small runtime.
FROM node:22-alpine AS ui
WORKDIR /src/ui
COPY ui/package*.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tar \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 10001 -m drawer \
    && mkdir -p /data /usr/share/drawer/ui /backups \
    && chown -R drawer:drawer /data /backups
COPY --from=builder /src/target/release/drawer /usr/local/bin/drawer
COPY --from=ui /src/ui/dist/ /usr/share/drawer/ui/
ENV DRAWER_ADDR=0.0.0.0:8787
ENV DRAWER_UI_DIR=/usr/share/drawer/ui
ENV DRAWER_DATA_DIR=/data
USER drawer
EXPOSE 8787
VOLUME /data
CMD ["drawer", "serve"]
