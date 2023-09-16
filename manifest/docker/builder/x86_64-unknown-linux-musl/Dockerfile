FROM messense/rust-musl-cross:x86_64-musl
RUN rustup toolchain install nightly --profile minimal -t x86_64-unknown-linux-musl
CMD ["/bin/bash"]
WORKDIR /home/rust/src