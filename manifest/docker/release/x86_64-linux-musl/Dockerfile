FROM messense/rust-musl-cross:x86_64-musl as builder
RUN rustup toolchain install nightly-x86_64-unknown-linux-musl --profile minimal -t x86_64-unknown-linux-musl
WORKDIR /app
COPY . .
RUN cargo +nightly build --release --target x86_64-unknown-linux-musl

FROM alpine:3.18
ENV TZ Asia/Shanghai
RUN apk add alpine-conf tzdata && \
    /sbin/setup-timezone -z Asia/Shanghai && \
    apk del alpine-conf

ENV WORKDIR /app
VOLUME $WORKDIR/data
ADD ./config.example.toml $WORKDIR/data/
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/jsdelivr_proxy $WORKDIR/jsdelivr_proxy
WORKDIR $WORKDIR

CMD ["./jsdelivr_proxy"]
