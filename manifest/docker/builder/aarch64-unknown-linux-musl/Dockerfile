FROM messense/rust-musl-cross:aarch64-musl
RUN rustup toolchain install nightly --profile minimal -t aarch64-unknown-linux-musl
CMD ["/bin/bash"]
WORKDIR /home/rust/src