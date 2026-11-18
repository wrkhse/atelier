# The two model checkers this experiment checks with, and nothing else.
# Both are installed here, inside CI only, at a pinned version and
# checksum: neither is a dependency of atelier, and neither runs on a
# developer's machine. specs/tla/README.md describes the experiment.
#
# The workflow builds this image and runs it with no network, a read-only
# root, a non-root user and no capabilities, so a check reads the spec and
# writes only a temporary folder.
FROM rust:1.100-bookworm@sha256:198cd28b4ba92b20fe849d5d8b119ace1bd28423d9e82e9c4de5ee1300d453a0

ENV LANG=C.UTF-8 LC_ALL=C.UTF-8

# TLC is a JVM program, and this image is Debian's.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends openjdk-17-jre-headless \
        ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# TLC, as tla2tools.jar from tlaplus/tlaplus v1.7.4, pinned by version and
# checksum. GitHub publishes no digest for that release's asset, so this
# checksum is the one the pinned asset had when it was pinned; an asset
# that changed fails this build instead of being checked.
ARG TLA2TOOLS_VERSION=v1.7.4
ARG TLA2TOOLS_SHA256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88

RUN mkdir -p /opt/tla \
    && curl --fail --silent --show-error --location \
        "https://github.com/tlaplus/tlaplus/releases/download/${TLA2TOOLS_VERSION}/tla2tools.jar" \
        --output /opt/tla/tla2tools.jar \
    && echo "${TLA2TOOLS_SHA256}  /opt/tla/tla2tools.jar" | sha256sum --check --strict -

# everystate, pinned by version and checksum: the .crate that crates.io
# published is verified against the checksum, unpacked, and built from that
# copy, so what runs is what the checksum names. The crate ships its
# Cargo.lock, so --locked pins its dependencies too.
ARG EVERYSTATE_VERSION=0.3.1
ARG EVERYSTATE_SHA256=6ff0b5c2bf92d65d6d2ac5606360bae137c87ec58a009a1808c6fee68ce5aaa5

RUN curl --fail --silent --show-error --location \
        "https://static.crates.io/crates/everystate/everystate-${EVERYSTATE_VERSION}.crate" \
        --output /tmp/everystate.crate \
    && echo "${EVERYSTATE_SHA256}  /tmp/everystate.crate" | sha256sum --check --strict - \
    && mkdir -p /tmp/everystate \
    && tar --extract --gzip --file /tmp/everystate.crate --directory /tmp/everystate \
    && cargo install --locked --path "/tmp/everystate/everystate-${EVERYSTATE_VERSION}" \
        --root /opt/everystate \
    && rm --recursive --force /tmp/everystate /tmp/everystate.crate

# A non-root user, as every gate container has.
RUN useradd --create-home --uid 10001 atelier

USER 10001

ENTRYPOINT ["/bin/bash", "/specs/check.sh"]
