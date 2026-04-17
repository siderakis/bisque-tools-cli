#!/bin/sh
set -e

REPO="siderakis/bisque-tools-cli"
INSTALL_DIR="${BISQUE_INSTALL:-$HOME/.bisque}/bin"

error() {
  echo "error: $*" >&2
  exit 1
}

# Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin) OS_TARGET="apple-darwin" ;;
  Linux)  OS_TARGET="unknown-linux-gnu" ;;
  *)      error "Unsupported OS: $OS" ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH_TARGET="x86_64" ;;
  arm64|aarch64) ARCH_TARGET="aarch64" ;;
  *)             error "Unsupported architecture: $ARCH" ;;
esac

TARGET="${ARCH_TARGET}-${OS_TARGET}"
ASSET_NAME="bisque-${TARGET}.tar.gz"

# Get latest release tag
LATEST_TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')

if [ -z "$LATEST_TAG" ]; then
  error "Could not determine latest release."
fi

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${LATEST_TAG}/${ASSET_NAME}"

echo "Installing bisque ${LATEST_TAG} (${TARGET})..."

# Download and extract
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

curl -fsSL "$DOWNLOAD_URL" -o "$TMPDIR/$ASSET_NAME" ||
  error "Failed to download bisque from $DOWNLOAD_URL"
tar xzf "$TMPDIR/$ASSET_NAME" -C "$TMPDIR" ||
  error "Failed to extract archive"

# Install
mkdir -p "$INSTALL_DIR" ||
  error "Failed to create install directory $INSTALL_DIR"
mv "$TMPDIR/bisque" "$INSTALL_DIR/bisque" ||
  error "Failed to install bisque to $INSTALL_DIR"
chmod +x "$INSTALL_DIR/bisque" ||
  error "Failed to set permissions on bisque executable"

echo "Installed bisque to $INSTALL_DIR/bisque"

# If bisque is already on PATH (reinstall), we're done
if command -v bisque >/dev/null 2>&1; then
  echo ""
  echo "Run 'bisque --help' to get started."
  exit
fi

# Add to PATH via shell config
SHELL_NAME="$(basename "$SHELL" 2>/dev/null || echo "sh")"

case "$SHELL_NAME" in
  fish)
    CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
    LINES='set --export BISQUE_INSTALL "$HOME/.bisque"
set --export PATH $BISQUE_INSTALL/bin $PATH'
    ;;
  bash)
    # Prefer .bashrc, fall back to .bash_profile
    CONFIG=""
    for f in "$HOME/.bashrc" "$HOME/.bash_profile"; do
      if [ -w "$f" ]; then
        CONFIG="$f"
        break
      fi
    done
    if [ -z "$CONFIG" ]; then
      CONFIG="$HOME/.bashrc"
    fi
    LINES='export BISQUE_INSTALL="$HOME/.bisque"
export PATH="$BISQUE_INSTALL/bin:$PATH"'
    ;;
  *)
    CONFIG="$HOME/.${SHELL_NAME}rc"
    LINES='export BISQUE_INSTALL="$HOME/.bisque"
export PATH="$BISQUE_INSTALL/bin:$PATH"'
    ;;
esac

# Only append if not already configured
if [ -f "$CONFIG" ] && grep -qF '.bisque/bin' "$CONFIG"; then
  : # already configured
elif [ -w "$CONFIG" ] || [ ! -f "$CONFIG" ]; then
  printf '\n# bisque\n%s\n' "$LINES" >> "$CONFIG"
  echo "Added bisque to \$PATH in $CONFIG"
else
  echo "Manually add to $CONFIG:"
  echo ""
  echo "  $LINES"
fi

echo ""
echo "To get started, run:"
echo ""
echo "  exec $SHELL"
echo "  bisque login"
echo "  bisque init"
