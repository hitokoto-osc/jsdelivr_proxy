FROM clux/muslrust:stable as builder
WORKDIR /app
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl

FROM alpine:3.18
ENV TZ Asia/Shanghai
RUN apk add alpine-conf tzdata && \
    /sbin/setup-timezone -z Asia/Shanghai && \
    apk del alpine-conf

ENV WORKDIR /app
VOLUME $WORKDIR/data
ADD config.example.toml $WORKDIR/data/
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/jsdelivr_proxy $WORKDIR/jsdelivr_proxy
WORKDIR $WORKDIR

CMD ["./jsdelivr_proxy"]
