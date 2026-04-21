FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /build
COPY Cargo.toml ./
COPY src/ src/
RUN cargo build --release

FROM alpine:3
COPY --from=builder /build/target/release/rdmatop /usr/local/bin/rdmatop
ENTRYPOINT ["/usr/local/bin/rdmatop"]
