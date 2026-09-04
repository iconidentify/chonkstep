# Native x86_64/aarch64 Arch package builder used by github-release.yml.
#
# The bootstrap shape is adapted from omacom/omarchy-pkgs' multi-architecture
# builder (MIT): Alpine's pacman creates a minimal target-architecture rootfs,
# then the final image builds as an unprivileged user.  The important property
# is that aarch64 packages are linked against Arch Linux ARM libraries on a
# native ARM runner, never cross-linked against Ubuntu.

FROM docker.io/alpine:3.21 AS bootstrap

ARG TARGETARCH

RUN apk add --no-cache bash curl gnupg pacman-makepkg zstd

RUN set -eux; \
    mkdir -p /etc/pacman.d /usr/share/pacman/keyrings; \
    case "$TARGETARCH" in \
      amd64) \
        package_arch=x86_64; \
        repositories='[core]\nInclude = /etc/pacman.d/mirrorlist\n\n[extra]\nInclude = /etc/pacman.d/mirrorlist\n'; \
        printf 'Server = https://geo.mirror.pkgbuild.com/$repo/os/$arch\n' > /etc/pacman.d/mirrorlist; \
        mkdir -p /tmp/archlinux-keyring; \
        curl -fsSL https://archlinux.org/packages/core/any/archlinux-keyring/download \
          | unzstd | tar -C /tmp/archlinux-keyring -x; \
        cp /tmp/archlinux-keyring/usr/share/pacman/keyrings/* /usr/share/pacman/keyrings/; \
        bootstrap_extra='' \
        ;; \
      arm64) \
        package_arch=aarch64; \
        repositories='[core]\nInclude = /etc/pacman.d/mirrorlist\n\n[extra]\nInclude = /etc/pacman.d/mirrorlist\n\n[alarm]\nInclude = /etc/pacman.d/mirrorlist\n\n[aur]\nInclude = /etc/pacman.d/mirrorlist\n'; \
        curl -fsSL https://raw.githubusercontent.com/archlinuxarm/PKGBUILDs/master/core/pacman-mirrorlist/mirrorlist \
          | sed -E 's/^[[:space:]]*#[[:space:]]*Server[[:space:]]*=/Server =/g; s/\$arch/aarch64/g' \
          > /etc/pacman.d/mirrorlist; \
        keyring_url=https://raw.githubusercontent.com/archlinuxarm/PKGBUILDs/master/core/archlinuxarm-keyring; \
        for file in archlinuxarm-revoked archlinuxarm-trusted archlinuxarm.gpg; do \
          curl -fsSL "$keyring_url/$file" -o "/usr/share/pacman/keyrings/$file"; \
        done; \
        bootstrap_extra=archlinuxarm-keyring \
        ;; \
      *) echo "unsupported Docker architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    printf '[options]\nHoldPkg = pacman glibc\nArchitecture = %s\nSigLevel = Required DatabaseOptional\nLocalFileSigLevel = Optional\n\n%b' \
      "$package_arch" "$repositories" > /etc/pacman.conf; \
    pacman-key --init; \
    pacman-key --populate; \
    install -d -m755 \
      /rootfs/var/cache/pacman/pkg /rootfs/var/lib/pacman /rootfs/var/log \
      /rootfs/dev /rootfs/run /rootfs/etc /rootfs/proc /rootfs/sys; \
    install -d -m1777 /rootfs/tmp; \
    pacman -r /rootfs -Sy --noconfirm base $bootstrap_extra; \
    cp /etc/pacman.conf /rootfs/etc/pacman.conf; \
    cp /etc/pacman.d/mirrorlist /rootfs/etc/pacman.d/mirrorlist; \
    rm -rf /rootfs/var/lib/pacman/sync/* /rootfs/var/cache/pacman/pkg/*

FROM scratch

ARG TARGETARCH

COPY --from=bootstrap /rootfs/ /

ENV LANG=C.UTF-8
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/lib/qt6/bin

RUN set -eux; \
    pacman-key --init; \
    if [ "$TARGETARCH" = arm64 ]; then \
      pacman-key --populate archlinux archlinuxarm; \
      pacman-key --lsign-key 77193F152BDBE6A6; \
    else \
      pacman-key --populate archlinux; \
    fi; \
    pacman -Syu --noconfirm; \
    pacman -S --needed --noconfirm \
      base-devel desktop-file-utils file git qt6-declarative rust sudo; \
    pacman -Scc --noconfirm; \
    rm -rf /var/cache/pacman/pkg/*; \
    useradd -m -s /bin/bash builder; \
    printf 'builder ALL=(ALL) NOPASSWD: ALL\n' > /etc/sudoers.d/builder; \
    chmod 440 /etc/sudoers.d/builder; \
    sed -i 's/^#MAKEFLAGS=.*/MAKEFLAGS="-j$(nproc)"/' /etc/makepkg.conf; \
    printf "\nPKGEXT='.pkg.tar.zst'\nCOMPRESSZST=(zstd -c -z -q --threads=0 -)\n" \
      >> /etc/makepkg.conf

USER builder
WORKDIR /build
