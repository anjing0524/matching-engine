# Build the single-process exchange runtime.
FROM rust:1.99-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
# Cache dependency compilation separately from source changes.
COPY src ./src
COPY benches ./benches
COPY examples ./examples
RUN cargo build --release --bin exchange

FROM debian:bookworm-slim
RUN useradd --system --create-home --uid 10001 exchange \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/exchange /usr/local/bin/exchange
USER exchange
EXPOSE 8000 9100
ENTRYPOINT ["/usr/local/bin/exchange", "run"]
CMD ["--listen", "0.0.0.0:8000", "--metrics-listen", "0.0.0.0:9100"]
