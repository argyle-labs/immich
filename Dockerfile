# TODO: base image + build for immich. Mirror jellyfin/Dockerfile conventions.
FROM debian:12-slim
LABEL org.opencontainers.image.source="https://github.com/argyle-labs/immich"
EXPOSE 2283
