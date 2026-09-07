FROM messense/rust-musl-cross:aarch64-musl as builder
# 国内网络环境构建时传入 --build-arg USE_RSPROXY=1，使用 rsproxy 镜像加速 rustup/crates
ARG USE_RSPROXY=""
RUN if [ -n "$USE_RSPROXY" ]; then \
        printf '[source.crates-io]\nreplace-with = "rsproxy-sparse"\n\n[source.rsproxy-sparse]\nregistry = "sparse+https://rsproxy.cn/index/"\n' > ${CARGO_HOME:-$HOME/.cargo}/config.toml; \
    fi
RUN if [ -n "$USE_RSPROXY" ]; then \
        export RUSTUP_DIST_SERVER="https://rsproxy.cn" RUSTUP_UPDATE_ROOT="https://rsproxy.cn/rustup"; \
    fi && \
    rustup update nightly && \
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
