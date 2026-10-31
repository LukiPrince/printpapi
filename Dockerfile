# 1) Build the dashboard. Next.js static export -> plain HTML/JS, so no Node in the runtime
#    image. crates/server/web is .dockerignore'd, so this stage is the only source of it.
# --platform=$BUILDPLATFORM: the export is architecture-independent, so the multi-arch release
# must not re-run this build under QEMU for every target.
FROM --platform=$BUILDPLATFORM node:22-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# 2) Build the server. Also on $BUILDPLATFORM: compiling Rust under QEMU takes many times longer
#    than cross-compiling, so an arm64 image is cross-built with the Debian cross toolchain.
FROM --platform=$BUILDPLATFORM rust:1.99-trixie AS build
ARG TARGETARCH
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) T=x86_64-unknown-linux-gnu;  G=x86-64 ;; \
      arm64) T=aarch64-unknown-linux-gnu; G=aarch64 ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    echo "$T" > /rust-target; \
    host=$(dpkg --print-architecture); \
    if [ "$host" != "$TARGETARCH" ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends "gcc-$G-linux-gnu" "libc6-dev-$TARGETARCH-cross"; \
      rm -rf /var/lib/apt/lists/*; \
    fi; \
    rustup target add "$T"
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo/ ./.cargo/
COPY crates/ ./crates/
COPY --from=web /web/out ./crates/server/web
# sharing=locked: buildx builds amd64 and arm64 in parallel, and cargo's own lock lives outside this
# mount, so two builds unpacking the same crate into it at once fail ("failed to unpack package").
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,id=target-$TARGETARCH \
    set -eux; \
    T=$(cat /rust-target); \
    if [ "$(dpkg --print-architecture)" != "$TARGETARCH" ]; then \
      CC_GNU="$(echo "$T" | sed 's/-unknown-/-/')-gcc"; \
      UP=$(echo "$T" | tr 'a-z-' 'A-Z_'); \
      export "CARGO_TARGET_${UP}_LINKER=$CC_GNU" "CC_$(echo "$T" | tr - _)=$CC_GNU"; \
    fi; \
    cargo build --release --locked -p printpapi-server --target "$T"; \
    install -D "target/$T/release/printpapi" /out/printpapi

# 3) The runtime: glibc + CA certificates and nothing else (no shell, no package manager).
#    Root, like the Python image was, so an existing data volume owned by root stays writable.
FROM gcr.io/distroless/cc-debian13
WORKDIR /app
COPY --from=build /out/printpapi /app/printpapi
EXPOSE 3460
ENTRYPOINT ["/app/printpapi"]
