# The production images, from the repository root:
#
#   podman build -f deploy/Dockerfile --target core      -t hd-core .
#   podman build -f deploy/Dockerfile --target assistant -t hd-assistant .
#   podman build -f deploy/Dockerfile --target zabbix    -t hd-zabbix .
#
# They share one Rust build stage, so building the second image reuses the first's work.

# The agents' UI, built into /src/web/dist.
FROM docker.io/library/node:24.22.3-trixie-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# The binaries. The caches keep downloaded crates and build output between builds on the same
# machine; they are not part of any image.
FROM docker.io/library/rust:1.100.0-slim-trixie AS build
# Limits parallel compile jobs on small build machines (empty: one per CPU).
ARG CARGO_BUILD_JOBS=
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core core
COPY sdk sdk
COPY addons addons
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    if [ -n "$CARGO_BUILD_JOBS" ]; then export CARGO_BUILD_JOBS; else unset CARGO_BUILD_JOBS; fi \
 && cargo build --release --locked -p hd-core -p hd-assistant -p hd-zabbix \
 && mkdir -p /out \
 && cp target/release/hd target/release/hd-assistant target/release/hd-zabbix /out/

# The core: the API, the UI, mail reading and sending. `hd serve` by default; any other `hd`
# command runs the same way (`hd customer add …`, `hd addon add …`).
FROM docker.io/library/debian:13.7-slim AS core
LABEL org.opencontainers.image.title="hd-core" \
      org.opencontainers.image.description="Help desk core: API, agents' UI, mail" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.source="https://github.com/manustars/appticket"
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN groupadd --system --gid 10001 hd \
 && useradd --system --uid 10001 --gid 10001 --home-dir /nonexistent --no-create-home \
      --shell /usr/sbin/nologin hd
COPY --from=build /out/hd /usr/local/bin/hd
COPY --from=web /src/web/dist /usr/share/hd/web
ENV HD_LISTEN=0.0.0.0:8080 \
    HD_WEB_DIR=/usr/share/hd/web
USER 10001:10001
EXPOSE 8080
ENTRYPOINT ["hd"]
CMD ["serve"]

# The Assistant add-on.
FROM docker.io/library/debian:13.7-slim AS assistant
LABEL org.opencontainers.image.title="hd-assistant" \
      org.opencontainers.image.description="Help desk Assistant add-on: proposals from a language model" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.source="https://github.com/manustars/appticket"
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN groupadd --system --gid 10001 hd \
 && useradd --system --uid 10001 --gid 10001 --home-dir /nonexistent --no-create-home \
      --shell /usr/sbin/nologin hd
COPY --from=build /out/hd-assistant /usr/local/bin/hd-assistant
ENV HD_ASSISTANT_LISTEN=0.0.0.0:8090
USER 10001:10001
EXPOSE 8090
ENTRYPOINT ["hd-assistant"]

# The Zabbix add-on. Its configuration file is named by HD_ZABBIX_CONFIG, which the chart and the
# Compose install mount; addon.listen there is 0.0.0.0:8091 in a container.
FROM docker.io/library/debian:13.7-slim AS zabbix
LABEL org.opencontainers.image.title="hd-zabbix" \
      org.opencontainers.image.description="Help desk Zabbix add-on: hosts as assets, tickets from alerts by rules" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.source="https://github.com/manustars/appticket"
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN groupadd --system --gid 10001 hd \
 && useradd --system --uid 10001 --gid 10001 --home-dir /nonexistent --no-create-home \
      --shell /usr/sbin/nologin hd
COPY --from=build /out/hd-zabbix /usr/local/bin/hd-zabbix
ENV HD_ZABBIX_CONFIG=/etc/hd/zabbix.toml
USER 10001:10001
EXPOSE 8091
ENTRYPOINT ["hd-zabbix"]
