# `cross` image for s390x-unknown-linux-musl, which has no prebuilt cross-rs
# image (it is a tier 3 Rust target). A GCC/musl cross toolchain is built with
# musl-cross-make on top of the s390x glibc image, which already provides
# QEMU for running tests.
FROM ghcr.io/cross-rs/s390x-unknown-linux-gnu:main

ARG MUSL_CROSS_MAKE_REV=227df8b99103f9c59f6570babf892978e293082f
ARG GCC_VER=13.3.0
ARG MUSL_VER=1.2.5

RUN apt-get update \
 && apt-get install --assume-yes --no-install-recommends \
        ninja-build patch python3 rsync xz-utils \
 && git clone https://github.com/richfelker/musl-cross-make.git /tmp/musl-cross-make \
 && cd /tmp/musl-cross-make \
 && git checkout "$MUSL_CROSS_MAKE_REV" \
 && make -j"$(nproc)" install \
        TARGET=s390x-linux-musl \
        GCC_VER="$GCC_VER" \
        MUSL_VER="$MUSL_VER" \
        OUTPUT=/usr/local \
        DL_CMD="curl -fsSL --retry 3 -o" \
        COMMON_CONFIG='CFLAGS="-g0 -O2" CXXFLAGS="-g0 -O2" LDFLAGS="-s"' \
        GCC_CONFIG="--enable-languages=c,c++ --disable-multilib" \
 && cd / \
 && rm -rf /tmp/musl-cross-make \
 # Rust's std links libunwind.a for static musl targets; GCC's libgcc_eh.a
 # implements the same _Unwind_* interface.
 && ln -s "$(s390x-linux-musl-gcc -print-file-name=libgcc_eh.a)" /usr/local/s390x-linux-musl/lib/libunwind.a

# The target links dynamically by default. musl installs its dynamic loader
# as an absolute symlink (to /lib/libc.so), which QEMU's sysroot prefix (-L)
# can't follow; make it relative.
RUN ln -sf libc.so /usr/local/s390x-linux-musl/lib/ld-musl-s390x.so.1

ENV CC_s390x_unknown_linux_musl=s390x-linux-musl-gcc \
    CXX_s390x_unknown_linux_musl=s390x-linux-musl-g++ \
    AR_s390x_unknown_linux_musl=s390x-linux-musl-ar \
    CARGO_TARGET_S390X_UNKNOWN_LINUX_MUSL_LINKER=s390x-linux-musl-gcc \
    CARGO_TARGET_S390X_UNKNOWN_LINUX_MUSL_RUNNER="qemu-s390x -L /usr/local/s390x-linux-musl"
