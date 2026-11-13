# Build from the repository root:
#   docker build -f apps/agent/Dockerfile -t vessel-agent .

FROM rust:1-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p vessel-agent

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data vessel \
 && mkdir -p /data && chown vessel:vessel /data
COPY --from=build /src/target/release/vessel-agent /usr/local/bin/vessel-agent

ENV VESSEL_BIND=0.0.0.0:8080 \
    VESSEL_DATABASE=/data/vessel.db \
    RUST_LOG=info
VOLUME /data
EXPOSE 8080
USER vessel
HEALTHCHECK --interval=30s --timeout=3s CMD ["/bin/bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q ok <&3"]
ENTRYPOINT ["/usr/local/bin/vessel-agent"]
