# Build the single-process trading runtime.
FROM rust:1.99-alpine AS builder
RUN apk add --no-cache musl-dev build-base
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY crates ./crates
COPY benches ./benches
COPY examples ./examples
RUN cargo build --release --bin trading-engine

FROM alpine:3.21
RUN adduser -D -u 10001 trading
COPY --from=builder /src/target/release/trading-engine /usr/local/bin/trading-engine
USER trading
EXPOSE 8000 9100
ENTRYPOINT ["/usr/local/bin/trading-engine", "run"]
CMD ["--listen", "0.0.0.0:8000", "--metrics-listen", "0.0.0.0:9100"]
