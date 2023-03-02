FROM docker.io/library/ubuntu:20.04

ARG RUNTIME=polkadot
ARG VCS_REF
ARG BUILD_DATE

LABEL description="Docker image for polkadot-introspector" \
	io.parity.image.vendor="Parity Technologies" \
	io.parity.image.description="Introspection in the chain progress from a birdseye-view" \
	io.parity.image.source="https://github.com/paritytech/polkadot-introspector/scripts/dockerfiles/polkadot-introspector_injected.Dockerfile" \
	io.parity.image.revision="${VCS_REF}" \
	io.parity.image.created="${BUILD_DATE}" \
	io.parity.image.documentation="https://github.com/paritytech/polkadot-introspector"

RUN mkdir -p /polkadot-introspector

COPY ./artifacts /polkadot-introspector

# Temporary installation of certificates
RUN apt-get update && apt-get install -y libssl1.1 ca-certificates && \
	useradd -m -u 1000 -U -s /bin/sh -d /polkadot-introspector polkadot-introspector && \
	rm -rf /var/lib/apt/lists/* && \
	# Sanity Check
	/polkadot-introspector/${RUNTIME}/polkadot-introspector --version

USER polkadot-introspector

ENV RUNTIME=$RUNTIME
ENTRYPOINT ["/polkadot-introspector/$RUNTIME/polkadot-introspector"]
