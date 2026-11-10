# Build the single-process exchange runtime.
FROM rust:1.99-alpine AS builder
RUN apk add --no-cache musl-dev build-base
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
COPY examples ./examples
RUN cargo build --release --no-default-features --bin exchange

FROM alpine:3.21
RUN adduser -D -u 10001 exchange
COPY --from=builder /src/target/release/exchange /usr/local/bin/exchange
USER exchange
EXPOSE 8000 9100
ENTRYPOINT ["/usr/local/bin/exchange", "run"]
CMD ["--listen", "0.0.0.0:8000", "--metrics-listen", "0.0.0.0:9100"]
