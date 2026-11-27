#!/bin/sh
set -eu

REPO="${ADBX_REPO:-qytech/adbx-rs}"
VERSION="${ADBX_VERSION:-latest}"
INSTALL_DIR="${ADBX_INSTALL_DIR:-$HOME/.local/bin}"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      x86_64) TARGET="x86_64-apple-darwin" ;;
      *) echo "Unsupported macOS architecture: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64|amd64) TARGET="x86_64-unknown-linux-musl" ;;
      arm64|aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
      *) echo "Unsupported Linux architecture: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS (use scripts/install.ps1 on Windows)" >&2
    exit 1
    ;;
esac

if [ "$VERSION" = "latest" ]; then
  REDIRECT_URL="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" || true)"
  VERSION="$(printf '%s' "$REDIRECT_URL" | sed -n 's|.*/releases/tag/\([^/?#]*\).*|\1|p')"
  if [ -z "$VERSION" ]; then
    VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  fi
  if [ -z "$VERSION" ]; then
    echo "Failed to resolve latest release tag for ${REPO}" >&2
    exit 1
  fi
fi

ARCHIVE="adbx-${VERSION}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading ${ARCHIVE} from ${REPO} (${VERSION})..."
curl -fsSL "${BASE_URL}/${ARCHIVE}" -o "${TMP_DIR}/${ARCHIVE}"
curl -fsSL "${BASE_URL}/SHA256SUMS.txt" -o "${TMP_DIR}/SHA256SUMS.txt"

EXPECTED="$(grep " ${ARCHIVE}\$" "${TMP_DIR}/SHA256SUMS.txt" | awk '{print $1}')"
if [ -z "$EXPECTED" ]; then
  echo "Checksum entry for ${ARCHIVE} not found in SHA256SUMS.txt" >&2
  exit 1
fi

if command -v shasum >/dev/null 2>&1; then
  ACTUAL="$(shasum -a 256 "${TMP_DIR}/${ARCHIVE}" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  ACTUAL="$(sha256sum "${TMP_DIR}/${ARCHIVE}" | awk '{print $1}')"
else
  echo "Neither shasum nor sha256sum found; cannot verify download" >&2
  exit 1
fi

if [ "$EXPECTED" != "$ACTUAL" ]; then
  echo "SHA256 mismatch! expected=$EXPECTED actual=$ACTUAL" >&2
  exit 1
fi

tar -xzf "${TMP_DIR}/${ARCHIVE}" -C "${TMP_DIR}"
mkdir -p "${INSTALL_DIR}"
install -m 755 "${TMP_DIR}/adbx" "${INSTALL_DIR}/adbx"

echo "Installed adbx to ${INSTALL_DIR}/adbx"
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *) echo "Add to PATH: export PATH=\"${INSTALL_DIR}:\$PATH\"" ;;
esac
if ! command -v adb >/dev/null 2>&1; then
  echo "Note: 'adb' was not found in PATH. Run 'adbx setup-adb' to auto-download official Android Platform-Tools."
fi
echo "Enable completion: source <(COMPLETE=zsh adbx)  # or COMPLETE=bash"
