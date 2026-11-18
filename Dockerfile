# syntax=docker/dockerfile:1.7
# V1 自建版镜像：Web 面板 + Rust 静态 musl 二进制，单镜像同时提供 server/client 子命令。
#
# 构建：
#   docker build -t meatus:local .
# 运行 server（配置与证书放在 /data）：
#   docker run --rm -p 7000:7000 -p 7400:7400 \
#     -v "$PWD/deploy/docker:/data" meatus:local server --config /data/server.toml
#
# 构建需要能拉取 node / rust / alpine 基础镜像；离线环境请用 scripts/build-release.sh
# 产出的静态二进制自行打包。国内网络可指定镜像：
#   docker build --build-arg BASE_REGISTRY=docker.m.daocloud.io/library/ \
#     --build-arg CARGO_REGISTRY=https://rsproxy.cn/index/ -t meatus:local .

ARG BASE_REGISTRY=docker.io/library/
ARG RUST_VERSION=1.96.0
ARG NODE_VERSION=22
# 可选 crates 镜像（如 https://rsproxy.cn/index/），用于 crates.io 不可达的环境
ARG CARGO_REGISTRY=""
ARG VERSION=1.1.0

FROM ${BASE_REGISTRY}node:${NODE_VERSION}-alpine AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM ${BASE_REGISTRY}rust:${RUST_VERSION}-alpine AS build
ARG CARGO_REGISTRY
RUN apk add --no-cache musl-dev
WORKDIR /src
# 不在镜像内复制 rust-toolchain.toml：由基础镜像固定工具链，避免 rustup 联网下载组件。
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY proto ./proto
COPY --from=web /src/web/dist ./web/dist
RUN if [ -n "$CARGO_REGISTRY" ]; then \
      mkdir -p .cargo && \
      printf '[source.crates-io]\nreplace-with = "mirror"\n\n[source.mirror]\nregistry = "sparse+%s"\n' "$CARGO_REGISTRY" > .cargo/config.toml; \
    fi
RUN cargo build --release --locked --bin meatus \
    && strip target/release/meatus

FROM ${BASE_REGISTRY}alpine:3.22
ARG VERSION=1.1.0
RUN apk add --no-cache ca-certificates tzdata su-exec \
    && addgroup -S meatus \
    && adduser -S -G meatus -h /data meatus
COPY --from=build /src/target/release/meatus /usr/local/bin/meatus
COPY config/server.example.toml config/client.example.toml /etc/meatus/
COPY config/server.example.yaml config/client.example.yaml /etc/meatus/
COPY LICENSE THIRD_PARTY_NOTICES.md /usr/share/doc/meatus/
COPY docs/THIRD_PARTY_LICENSES.md docs/THIRD_PARTY_LICENSE_TEXTS.txt /usr/share/doc/meatus/
COPY deploy/docker/entrypoint.sh /usr/local/bin/meatus-entrypoint
RUN chmod 0755 /usr/local/bin/meatus-entrypoint \
    && mkdir -p /data/certs /data/acme \
    && chown -R meatus:meatus /data /etc/meatus
# 入口以 root 修正 /data 权限后，通过 su-exec 降权到 meatus 运行；
# 这样 bind mount 与命名卷都不会因宿主机 UID 不同而写入失败。
WORKDIR /data
STOPSIGNAL SIGTERM
EXPOSE 7000 7400 80 443 2080
LABEL org.opencontainers.image.title="meatus" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.description="Self-hosted reverse tunnel: server + client + web panel" \
      org.opencontainers.image.licenses="Apache-2.0"
ENTRYPOINT ["/usr/local/bin/meatus-entrypoint"]
CMD ["server", "--config", "/data/server.toml"]
