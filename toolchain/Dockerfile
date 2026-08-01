# The container every published binary is built in.
#
# Kira's whole claim is that a binary can be rebuilt from a named source by a
# published recipe, and the compiler is part of that recipe. So the toolchain has
# to be one artifact, pinned by digest, holding everything any app might need to
# build -- an app that needs a tool the image lacks cannot be built here at all,
# and installing one inside the job would make the recipe's toolchain digest
# describe something other than what actually did the compiling.
#
# Owning the image also removes a single point of failure: reproducibility
# previously depended on a third party's Docker tag staying available.

# The ARM toolchain, pinned. Same image the catalogue has always been built with,
# so binaries are unchanged by the move except for what is added below.
FROM xanderhendriks/stm32cubeide:16.0@sha256:7e07c508e3944def22eaabe822eaf902a2ba4fbb38a3ce24d6ff874f9f04c447

# Ubuntu's security mirror has hung builds before; the main archive carries the
# same packages.
RUN find /etc/apt -type f \( -name '*.list' -o -name '*.sources' \) \
      -exec sed -i 's#security\.ubuntu\.com/ubuntu#archive.ubuntu.com/ubuntu#g' {} + || true \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      bash git curl ca-certificates jq \
      cmake \
      python3-full python3-pip python-is-python3 \
 && rm -rf /var/lib/apt/lists/*

# The image ships these unexecutable, and every job had to fix that itself.
RUN for tool in make arm-none-eabi-gcc arm-none-eabi-g++ arm-none-eabi-objcopy arm-none-eabi-size; do \
      path="$(find /opt/st/stm32cubeide_* -type f -name "$tool" | head -n 1)"; \
      [ -n "$path" ] && chmod +x "$path" || true; \
    done

# Put the ARM tools and the bundled make on PATH once, rather than in each job.
ENV PATH="/opt/st/toolchain/bin:${PATH}"
RUN mkdir -p /opt/st/toolchain/bin \
 && for tool in arm-none-eabi-gcc arm-none-eabi-g++ arm-none-eabi-objcopy arm-none-eabi-size arm-none-eabi-objdump make; do \
      path="$(find /opt/st/stm32cubeide_* -type f -name "$tool" | head -n 1)"; \
      [ -n "$path" ] && ln -sf "$path" "/opt/st/toolchain/bin/$tool" || true; \
    done

# Rust, for apps whose sources are not all C. Pinned exactly: "stable" would make
# the image's contents depend on when it was built, and every binary built here
# would inherit that.
ENV RUSTUP_HOME=/opt/rust/rustup \
    CARGO_HOME=/opt/rust/cargo \
    PATH=/opt/rust/cargo/bin:/opt/st/toolchain/bin:${PATH}
ARG RUST_VERSION=1.97.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --profile minimal \
        --default-toolchain "${RUST_VERSION}" \
        --target thumbv8m.main-none-eabihf \
        --target thumbv7em-none-eabihf \
 && chmod -R a+rX /opt/rust \
 && rustc --version && cargo --version

# Cargo does reach the network, because a crate's dependencies are not in the
# image. That is tolerable rather than ideal: Cargo.lock pins each dependency to
# an exact version and checksum, so the fetch is deterministic as long as the app
# commits its lock file. An app that does not is not reproducible here, whatever
# this image does.

LABEL org.opencontainers.image.source=https://github.com/tobymurray/kira \
      org.opencontainers.image.description="ARM toolchain plus Rust, for building UNA Watch apps reproducibly" \
      org.opencontainers.image.licenses=MIT
