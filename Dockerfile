# syntax=docker.io/docker/dockerfile:1

# This enforces that the packages downloaded from the repositories are the same
# for the defined date, no matter when the image is built.
ARG UBUNTU_TAG=noble-20260210.1
ARG APT_UPDATE_SNAPSHOT=20260215T030400Z
ARG MACHINE_GUEST_TOOLS_VERSION=0.17.2
ARG MACHINE_GUEST_TOOLS_SHA256SUM=c077573dbcf0cdc146adf14b480bfe454ca63aa4d3e8408c5487f550a5b77a41
ARG LINUX_HEADERS_URLPATH=https://github.com/cartesi/machine-linux-image/releases/download/v0.20.0/linux-libc-dev-riscv64-cross-6.5.13-ctsi-1-v0.20.0.deb
ARG LINUX_HEADERS_SHA256=2723435e8b45d8fb7a79e9344f6dc517b3dbc08e03ac17baab311300ec475c08
ARG RUST_VERSION=1.90.0

################################################################################
# riscv64 base stage
FROM --platform=linux/riscv64 ubuntu:${UBUNTU_TAG} AS base-riscv64

ARG APT_UPDATE_SNAPSHOT
ARG DEBIAN_FRONTEND=noninteractive
RUN <<EOF
set -eu
apt-get update
apt-get install -y --no-install-recommends ca-certificates
apt-get update --snapshot=${APT_UPDATE_SNAPSHOT}
apt-get remove -y --purge ca-certificates
apt-get autoremove -y --purge
EOF

################################################################################
# cross base stage
FROM --platform=$BUILDPLATFORM ubuntu:${UBUNTU_TAG} AS base-cross

ARG APT_UPDATE_SNAPSHOT
ARG DEBIAN_FRONTEND=noninteractive
RUN <<EOF
set -eu
apt-get update
apt-get install -y --no-install-recommends ca-certificates
apt-get update --snapshot=${APT_UPDATE_SNAPSHOT}
apt-get remove -y --purge ca-certificates
apt-get autoremove -y --purge
EOF

################################################################################
# cross build stage
FROM base-cross AS cross-build-stage

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

ARG DEBIAN_FRONTEND=noninteractive
RUN <<EOF
set -eu
apt-get update --snapshot=${APT_UPDATE_SNAPSHOT}
apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    libclang-dev \
    g++-riscv64-linux-gnu \
    make \
    gcc \
    g++ \
    libc6-dev
rm -rf /var/lib/apt/lists/*
EOF

ARG RUST_VERSION
RUN <<EOF
set -eux
dpkgArch="$(dpkg --print-architecture)"
case "${dpkgArch##*-}" in \
    amd64) rustArch='x86_64-unknown-linux-gnu'; rustupSha256='20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c' ;;
    armhf) rustArch='armv7-unknown-linux-gnueabihf'; rustupSha256='3b8daab6cc3135f2cd4b12919559e6adaee73a2fbefb830fadf0405c20231d61' ;;
    arm64) rustArch='aarch64-unknown-linux-gnu'; rustupSha256='e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c' ;;
    i386) rustArch='i686-unknown-linux-gnu'; rustupSha256='a5db2c4b29d23e9b318b955dd0337d6b52e93933608469085c924e0d05b1df1f' ;;
    *) echo >&2 "unsupported architecture: ${dpkgArch}"; exit 1 ;;
esac
url="https://static.rust-lang.org/rustup/archive/1.28.2/${rustArch}/rustup-init"
curl -fsSL -O "$url"
echo "${rustupSha256} *rustup-init" | sha256sum -c -
chmod +x rustup-init
./rustup-init -y --no-modify-path --profile minimal --default-toolchain ${RUST_VERSION} --default-host ${rustArch}
rm rustup-init
chmod -R a+w $RUSTUP_HOME $CARGO_HOME
rustup --version
cargo --version
rustc --version
EOF

RUN rustup target add riscv64gc-unknown-linux-gnu

WORKDIR /opt/cartesi/dapp
COPY . .

# libcmt-binding-rust generates bindings only. Build and link libcmt explicitly.
ARG LINUX_HEADERS_URLPATH
ARG LINUX_HEADERS_SHA256
RUN echo "[headers] Downloading Cartesi linux headers..." && \
    curl -fsSL -o /tmp/linux-headers.deb "${LINUX_HEADERS_URLPATH}" && \
    echo "${LINUX_HEADERS_SHA256} /tmp/linux-headers.deb" | sha256sum -c - && \
    dpkg-deb -x /tmp/linux-headers.deb / && \
    rm /tmp/linux-headers.deb

RUN echo "[libcmt] Cloning machine-guest-tools..." && \
    git clone --depth 1 https://github.com/cartesi/machine-guest-tools.git /tmp/machine-guest-tools
RUN echo "[libcmt] Building libcmt for riscv64..." && \
    make -C /tmp/machine-guest-tools/sys-utils/libcmt libcmt TOOLCHAIN_PREFIX=riscv64-linux-gnu-
RUN mkdir -p /opt/cartesi/dapp/libcmt-build && \
    cp /tmp/machine-guest-tools/sys-utils/libcmt/build/riscv64/libcmt.a /opt/cartesi/dapp/libcmt-build/

ENV RUSTFLAGS="-L /opt/cartesi/dapp/libcmt-build -l static=cmt"

RUN cargo build --release --target riscv64gc-unknown-linux-gnu

################################################################################
# runtime stage: produces final image that will be executed
FROM base-riscv64

ARG MACHINE_GUEST_TOOLS_VERSION
ARG MACHINE_GUEST_TOOLS_SHA256SUM
ADD --checksum=sha256:${MACHINE_GUEST_TOOLS_SHA256SUM} \
  https://github.com/cartesi/machine-guest-tools/releases/download/v${MACHINE_GUEST_TOOLS_VERSION}/machine-guest-tools_riscv64.deb \
  /tmp/machine-guest-tools_riscv64.deb

ARG DEBIAN_FRONTEND=noninteractive
RUN <<EOF
set -e
apt-get install -y --no-install-recommends \
    busybox-static \
    /tmp/machine-guest-tools_riscv64.deb

rm /tmp/machine-guest-tools_riscv64.deb
rm -rf /var/lib/apt/lists/* /var/log/* /var/cache/*
EOF

ENV PATH="/opt/cartesi/bin:/opt/cartesi/dapp:${PATH}"

WORKDIR /opt/cartesi/dapp
COPY --from=cross-build-stage /opt/cartesi/dapp/target/riscv64gc-unknown-linux-gnu/release/cma-rust-wallet .

ENV ROLLUP_HTTP_SERVER_URL="http://127.0.0.1:5004"

ENTRYPOINT ["cma-rust-wallet"]
