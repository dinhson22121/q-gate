# qgate: one container that watches bind-mounted git repos, reviews new commits with Claude and serves a
# dashboard. Language toolchains (Go, Java, ...) are NOT baked in: docker/entrypoint.sh installs the ones
# chosen in qgate.env into the /toolchains volume on first start (see README, "Toolchains").
#
# Build args:
#   QGATE_UID / QGATE_GID   uid/gid of the runtime user (default 1000:1000); keep equal to compose `user:`
#   QGATE_PREINSTALL        comma list of toolchains to bake in at build time (CI / offline), e.g. go,java
#   QGATE_APT_PACKAGES      extra Debian packages, e.g. "build-essential" for Rust/C-extension builds
#   QGATE_CLAUDE_VERSION    pinned Claude Code CLI release baked into the `qgate` target (not the runner)
#
# Targets: `qgate` (default) = `runner` + the `claude` CLI. The runner image has no `claude` and no token.

############################
# Builder
############################
FROM rust:1-slim-bookworm AS builder
WORKDIR /src

# Dependency layer: compiled against a stub crate, so it is rebuilt only when Cargo.toml/Cargo.lock change.
# (Plain layer caching instead of BuildKit cache mounts: works with the classic builder too.)
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/.fingerprint/qgate-* target/release/deps/qgate-* target/release/deps/libqgate-*

COPY src ./src
# Copied sources keep their (older) host mtimes, so force cargo to rebuild the crate instead of
# reusing the stub binary, then fail the build if the result is not the real qgate.
RUN touch src/main.rs src/lib.rs \
    && rm -f target/release/qgate \
    && cargo build --release --locked --bin qgate \
    && target/release/qgate --version | grep -q '^qgate ' \
    && install -D -m 0755 target/release/qgate /out/qgate

############################
# Runner (no Claude CLI) - also the base of the qgate image
############################
FROM debian:bookworm-slim AS runner

ARG QGATE_UID=1000
ARG QGATE_GID=1000
ARG QGATE_APT_PACKAGES=""

# jq parses vendor release metadata; unzip unpacks Gradle; xz-utils unpacks Node.js.
# QGATE_APT_PACKAGES is word-split on purpose (a list of package names).
# hadolint ignore=DL3008
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git jq tini unzip xz-utils ${QGATE_APT_PACKAGES} \
    && rm -rf /var/lib/apt/lists/* \
    && git config --system --add safe.directory '*'

# Reuse an existing group when the gid is taken (e.g. macOS gid 20 = Debian "dialout").
RUN if ! getent group "${QGATE_GID}" >/dev/null; then groupadd --gid "${QGATE_GID}" qgate; fi \
    && useradd --uid "${QGATE_UID}" --gid "${QGATE_GID}" --no-create-home --home-dir /data/home \
        --shell /usr/sbin/nologin qgate \
    && mkdir -p /data/home /config /toolchains /repos /work /cache \
    && chown "${QGATE_UID}:${QGATE_GID}" /data /data/home /config /toolchains /work /cache

COPY --from=builder /out/qgate /usr/local/bin/qgate
COPY docker/entrypoint.sh /opt/qgate/entrypoint.sh
COPY docker/toolchains/ /opt/qgate/toolchains/
COPY config/qgate.toml.example /opt/qgate/qgate.toml.example
COPY eval/cases /usr/share/qgate/eval/cases
# Strip CRLF in case the repo was checked out on Windows with autocrlf.
RUN sed -i 's/\r$//' /opt/qgate/entrypoint.sh /opt/qgate/toolchains/*.sh \
    && chmod 0755 /opt/qgate/entrypoint.sh /opt/qgate/toolchains/*.sh \
    && ln -s /opt/qgate/toolchains/qgate-toolchains.sh /usr/local/bin/qgate-toolchains

# Static toolchain locations, set here so `docker compose exec` shells see them as well.
# Missing directories on PATH are harmless; docker/toolchains/env.sh unsets JAVA_HOME/MAVEN_HOME if absent.
ENV QGATE_DATA_DIR=/data \
    QGATE_CONFIG_DIR=/config \
    QGATE_REPOS_ROOT=/repos \
    QGATE_TOOLCHAINS_DIR=/toolchains \
    QGATE_BIND=0.0.0.0:8080 \
    HOME=/data/home \
    XDG_CACHE_HOME=/data/cache \
    GOPATH=/data/go \
    GOCACHE=/data/cache/go-build \
    JAVA_HOME=/toolchains/java/current \
    MAVEN_HOME=/toolchains/maven/current \
    GRADLE_USER_HOME=/data/cache/gradle \
    npm_config_cache=/data/cache/npm \
    COREPACK_HOME=/data/cache/corepack \
    UV_CACHE_DIR=/data/cache/uv \
    UV_PYTHON_INSTALL_DIR=/toolchains/python/.uv-pythons \
    UV_PYTHON_BIN_DIR=/toolchains/python/.uv-bin \
    PIP_CACHE_DIR=/data/cache/pip \
    RUSTUP_HOME=/toolchains/rust/rustup \
    CARGO_HOME=/toolchains/rust/cargo \
    PATH=/toolchains/go/current/bin:/data/go/bin:/toolchains/java/current/bin:/toolchains/maven/current/bin:/toolchains/gradle/current/bin:/toolchains/node/current/bin:/toolchains/python/current/bin:/toolchains/uv/current:/toolchains/rust/cargo/bin:/toolchains/gitleaks/current/bin:/toolchains/osv-scanner/current/bin:/toolchains/govulncheck/current/bin:/toolchains/golangci-lint/current/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

USER ${QGATE_UID}:${QGATE_GID}

# Optional: bake toolchains into the image (same scripts, checksums enforced, build fails on error).
# Must run before VOLUME, otherwise the files would be discarded.
ARG QGATE_PREINSTALL=""
RUN if [ -n "${QGATE_PREINSTALL}" ]; then \
        HOME=/tmp QGATE_TOOLCHAINS="${QGATE_PREINSTALL}" bash /opt/qgate/toolchains/install.sh --strict \
        && rm -rf /toolchains/.tmp /toolchains/.lock; \
    fi

# /work (commit snapshots) and /cache (runner build caches) are compose named volumes, seeded from the
# owned directories above.
VOLUME ["/data", "/config", "/toolchains"]
EXPOSE 8080

# Long start period: the first start may download toolchains before the dashboard comes up.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15m --start-interval=5s --retries=3 \
    CMD curl -fsS -o /dev/null "http://127.0.0.1:${QGATE_BIND##*:}/healthz" || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/opt/qgate/entrypoint.sh"]
CMD ["qgate", "serve"]

############################
# qgate: the runner image plus the Claude Code CLI (default target)
############################
FROM runner AS qgate

ARG QGATE_UID=1000
ARG QGATE_GID=1000
# Pinned release of the native Claude Code binary; the download is verified against the release manifest's
# SHA-256. Bump it deliberately (and re-run the tests): the CLI's flags and output are an interface of qgate.
ARG QGATE_CLAUDE_VERSION=2.1.330

USER root
# hadolint ignore=DL4006
RUN set -eu; \
    case "$(dpkg --print-architecture)" in \
        amd64) platform=linux-x64 ;; \
        arm64) platform=linux-arm64 ;; \
        *) echo "unsupported architecture for the Claude CLI" >&2; exit 1 ;; \
    esac; \
    base="https://downloads.claude.ai/claude-code-releases/${QGATE_CLAUDE_VERSION}"; \
    sum="$(curl -fsSL "${base}/manifest.json" | jq -r ".platforms[\"${platform}\"].checksum")"; \
    curl -fsSL -o /usr/local/bin/claude "${base}/${platform}/claude"; \
    echo "${sum}  /usr/local/bin/claude" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/claude; \
    claude --version | grep -q "${QGATE_CLAUDE_VERSION}"
USER ${QGATE_UID}:${QGATE_GID}
