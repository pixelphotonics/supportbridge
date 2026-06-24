FROM rust:1-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y libssl-dev pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
# Pre-build dependencies with a stub binary so the heavy compile step is cached
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/supportbridge /usr/local/bin/supportbridge
EXPOSE 8091
ENTRYPOINT ["supportbridge"]
CMD ["serve"]
