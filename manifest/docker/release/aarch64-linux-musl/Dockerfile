FROM messense/rust-musl-cross:aarch64-musl as builder
RUN rustup update nightly && \
    rustup target add --toolchain nightly aarch64-unknown-linux-musl
WORKDIR /app
COPY . .
RUN cargo +nightly build --release --target aarch64-unknown-linux-musl

FROM alpine:3.18
ENV TZ Asia/Shanghai
RUN apk add alpine-conf tzdata && \
    /sbin/setup-timezone -z Asia/Shanghai && \
    apk del alpine-conf

ENV WORKDIR /app
VOLUME $WORKDIR/data
ADD ./config.example.toml $WORKDIR/data/
COPY --from=builder /app/target/aarch64-unknown-linux-musl/release/jsdelivr_proxy $WORKDIR/jsdelivr_proxy
WORKDIR $WORKDIR

CMD ["./jsdelivr_proxy"]
