FROM messense/rust-musl-cross:aarch64-musl
RUN rustup toolchain install nightly-x86_64-unknown-linux-musl --profile minimal -t aarch64-unknown-linux-musl
CMD ["/bin/bash"]
WORKDIR /home/rust/src