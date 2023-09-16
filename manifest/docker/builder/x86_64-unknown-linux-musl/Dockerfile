FROM messense/rust-musl-cross:x86_64-musl
RUN rustup update nightly && \
    rustup target add --toolchain nightly x86_64-unknown-linux-musl
CMD ["/bin/bash"]
WORKDIR /home/rust/src