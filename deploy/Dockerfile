# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    RUSTUP_TOOLCHAIN=stable cargo build --release -p cs-server \
    && cp target/release/cognitive-substrate /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 substrate
COPY --from=build /usr/local/bin/cognitive-substrate /usr/local/bin/cognitive-substrate
USER substrate
ENV CS_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["cognitive-substrate"]
CMD ["serve"]
