# Gate tooling only: no project code, credentials or Docker socket enter this
# image. Every tool is pinned; the gates run as user 10001.
FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea

RUN apt-get update && apt-get install --yes --no-install-recommends \
    ca-certificates curl git jq python3 xz-utils \
    && rm -rf /var/lib/apt/lists/*

# moon 2.6.5, checked against its published SHA-256 before it is installed.
ARG MOON_VERSION=2.6.5
ARG MOON_ARCHIVE=moon_cli-x86_64-unknown-linux-gnu.tar.xz
RUN set -eu; \
    cd /tmp; \
    url="https://github.com/moonrepo/moon/releases/download/v${MOON_VERSION}"; \
    curl --fail --silent --show-error --location --remote-name "$url/$MOON_ARCHIVE"; \
    curl --fail --silent --show-error --location --remote-name "$url/$MOON_ARCHIVE.sha256"; \
    echo "$(cut -d ' ' -f 1 "$MOON_ARCHIVE.sha256")  $MOON_ARCHIVE" | sha256sum -c -; \
    mkdir moon; \
    tar -xJf "$MOON_ARCHIVE" -C moon; \
    install -m 0755 "$(find moon -type f -name moon | head -n 1)" /usr/local/bin/moon; \
    rm -rf moon "$MOON_ARCHIVE" "$MOON_ARCHIVE.sha256"; \
    moon --version

# Miri runs only on nightly; the miri gate alone uses this pinned one.
RUN rustup component add clippy rustfmt rust-src \
    && rustup toolchain install nightly-2026-08-21 --profile minimal \
       --component miri,rust-src \
    && cargo install --locked cargo-nextest@0.9.152 cargo-mutants@27.1.0 \
    && cargo install --locked kani-verifier --version 0.68.0 \
    && cargo install --locked rust-code-analysis-cli --version 0.0.25 \
    && rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git" \
    && chmod -R a+w "$CARGO_HOME" "$RUSTUP_HOME"

# /reports is outside the copied checkout, so no committed file can sit there.
# /work-base holds metrics/complexity's worktree of the base commit.
RUN useradd --uid 10001 --create-home ci \
    && install -d -o ci -g ci /work /work-base /reports
COPY entrypoint.sh /opt/ci/entrypoint.sh
RUN chmod 0555 /opt/ci/entrypoint.sh

ENV HOME=/home/ci \
    PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin
USER 10001:10001
# Only Kani's bundle is prebuilt, in the gate user's home. Miri's sysroot is
# built by the gate task with its MIRIFLAGS.
RUN cargo kani setup
WORKDIR /work
ENTRYPOINT ["/opt/ci/entrypoint.sh"]
