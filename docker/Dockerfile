FROM docker.io/paritytech/ci-linux:production as builder

WORKDIR /polkadot-introspector
COPY . /polkadot-introspector
RUN cargo build --release


FROM docker.io/library/ubuntu:20.04
LABEL description="Multistage Docker image for polkadot-introspector" \
	io.parity.image.vendor="Parity Technologies" \
	io.parity.image.description="Introspection in the chain progress from a birdseye-view" \
	io.parity.image.source="https://github.com/paritytech/polkadot-introspector/docker/Dockerfile" \
	io.parity.image.documentation="https://github.com/paritytech/polkadot-introspector"


COPY --from=builder /polkadot-introspector/target/release/polkadot-introspector /usr/local/bin

# Temporary installation of certificates
RUN apt-get update && apt-get install -y libssl1.1 ca-certificates && \
    useradd -m -u 1000 -U -s /bin/sh -d /polkadot-introspector polkadot-introspector && \
    rm -rf /var/lib/apt/lists/* && \
    # Sanity Check
	/usr/local/bin/polkadot-introspector --version

USER polkadot-introspector


ENTRYPOINT ["/usr/local/bin/polkadot-introspector"]
