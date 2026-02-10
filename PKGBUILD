# Maintainer: Your Name <youremail@domain.com>
pkgname=btrbak
pkgver=0.1.0
pkgrel=2
pkgdesc="Incremental Btrfs backups with live boot environment and LUKS encryption support"
arch=('x86_64' 'aarch64')
url="https://github.com/owlsome2501/backup-btrfs"
license=('MIT')
depends=('btrfs-progs' 'util-linux')
optdepends=('cryptsetup: LUKS encryption target support'
            'snapper: snapshot management with use_snapper = true'
            'systemd: systemd-boot support for live boot (bootctl)')
makedepends=('cargo' 'git')
source=("git+https://github.com/owlsome2501/backup-btrfs.git")
sha256sums=('SKIP')

pkgver() {
  cd "$srcdir/$pkgname"
  grep -oP '^version = "\K[^"]+' Cargo.toml
}

prepare() {
  cd "$srcdir/$pkgname"
  cargo fetch --locked --target "$CARCH-unknown-linux-gnu"
}

build() {
  cd "$srcdir/$pkgname"
  cargo build --frozen --release --all-features
}

check() {
  cd "$srcdir/$pkgname"
  cargo test --frozen --all-features
}

package() {
  cd "$srcdir/$pkgname"
  install -Dm0755 -t "$pkgdir/usr/bin/" "target/release/$pkgname"

  # Install documentation
  install -Dm0644 -t "$pkgdir/usr/share/doc/$pkgname/" README.md

  # Install license files
  for license in LICENSE*; do
    if [[ -f "$license" ]]; then
      install -Dm0644 "$license" "$pkgdir/usr/share/licenses/$pkgname/$license"
    fi
  done

  # Install configuration example
  install -Dm0644 btrbak.toml "$pkgdir/usr/share/doc/$pkgname/examples/btrbak.toml" 2>/dev/null || true
}
