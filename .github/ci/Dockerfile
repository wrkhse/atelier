# Tooling only: no project code, credentials or Docker socket enter this image.
FROM node:24-bookworm-slim@sha256:dc26ae92c9fb15cf6b1d63e20f0944b2efe0f115b20e00be48fb23fceb061c54 AS node
FROM golang:1.25-bookworm@sha256:3b4a11519ad929d1e1d261a12cff056f0c85b735253d7d861346b9c6f8b36437 AS go
FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea

# TOOLS=1 carries the gates' pinned tools; the planner and every fallback
# build use TOOLS=0, which installs none of them.
ARG TOOLS=1

# shellcheck keeps actionlint's run-script checks, as on the hosted runner.
RUN apt-get update && apt-get install --yes --no-install-recommends \
    ca-certificates curl git jq libssl-dev pkg-config procps python3 python3-venv \
    shellcheck unzip \
    && rm -rf /var/lib/apt/lists/*
# Debian 12's pipx 1.1.0 cannot run `ruff==VERSION`; pin a current one. The
# ruff gate's own version is baked in, so the self-test needs no index.
RUN python3 -m venv /opt/pipx \
    && /opt/pipx/bin/pip install --no-cache-dir pipx==1.7.1 ruff==0.17.4 \
    && ln -s /opt/pipx/bin/pipx /usr/local/bin/pipx
COPY --from=node /usr/local/bin /usr/local/bin
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
COPY --from=go /usr/local/go /usr/local/go
RUN rustup component add rustfmt clippy

# Each gate's setup installs its pinned tool with cargo, which skips a package
# already installed at the same version; with TOOLS=1 the image carries those
# exact versions from the same commands, so each setup line is a no-op. Every
# install is asserted, so a broken tool layer fails this image, not a gate.
# The nextest tarball that nextest and mutants fetch is cheap and stays out of
# the image. chmod repeats the base image's a+w for the files root just wrote,
# so the gate user can still update cargo's lock, tracker and caches.
RUN if [ "$TOOLS" = 1 ]; then \
      cargo install --locked cargo-deny@0.20.2 \
      && cargo install --locked cargo-shear@1.14.0 \
      && cargo install --locked cargo-mutants@27.1.0 \
      && cargo install --locked kani-verifier --version 0.68.0 \
      && cargo install --list | grep -Fqx 'cargo-deny v0.20.2:' \
      && cargo install --list | grep -Fqx 'cargo-shear v1.14.0:' \
      && cargo install --list | grep -Fqx 'cargo-mutants v27.1.0:' \
      && cargo install --list | grep -Fqx 'kani-verifier v0.68.0:' \
      && chmod -R a+w "$CARGO_HOME"; \
    fi

# /reports is outside the copied checkout, so no committed file can sit there.
RUN useradd --uid 10001 --create-home ci && install -d -o ci -g ci /work /reports
COPY cargo.py /opt/ci/bin/cargo
COPY entrypoint.sh /opt/ci/entrypoint.sh
COPY reports.py shards.py test_reports.py contained.jq floor.sh inspection.json /opt/ci/
RUN chmod 0555 /opt/ci/bin/cargo /opt/ci/entrypoint.sh

ENV HOME=/home/ci \
    PATH=/opt/ci/bin:/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/bin:/usr/bin:/bin
USER 10001:10001
# kani's first-time setup as the gate user puts CBMC and the Kani bundle in
# /home/ci/.kani. "cargo kani setup" always downloads — kani's proxy treats an
# explicit setup as an explicit setup — so the kani gate still pays that
# download; the image saves only its cargo install. A failed setup fails this
# build.
RUN if [ "$TOOLS" = 1 ]; then /usr/local/cargo/bin/cargo-kani setup; fi
WORKDIR /work
ENTRYPOINT ["/opt/ci/entrypoint.sh"]
