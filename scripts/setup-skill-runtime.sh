#!/usr/bin/env bash
# One entry point for checkout and packaged setup; credentials are separate.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" == Darwin ]]; then
  export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
fi
if [[ ! -f "$ROOT_DIR/skillshub/Makefile" ]]; then
  echo "This package lacks its skill runtime payload. Install a complete release." >&2
  exit 1
fi
missing_formulas=()
command -v pdftotext >/dev/null && command -v pdftoppm >/dev/null || missing_formulas+=(poppler)
command -v tesseract >/dev/null || missing_formulas+=(tesseract)
if [[ -f "$ROOT_DIR/Cargo.toml" ]] && ! command -v go >/dev/null; then
  missing_formulas+=(go)
fi
if [[ ${#missing_formulas[@]} -gt 0 ]]; then
  if [[ "$(uname -s)" == Darwin ]] && command -v brew >/dev/null; then
    HOMEBREW_NO_AUTO_UPDATE=1 brew install "${missing_formulas[@]}"
  else
    echo "Install these host tool prerequisites, then retry: ${missing_formulas[*]}" >&2
    exit 1
  fi
fi
if [[ ! -f "$ROOT_DIR/Makefile" ]]; then
  python3 "$ROOT_DIR/skillshub/scripts/stage_release.py" --verify "$ROOT_DIR/skillshub"
fi
make -C "$ROOT_DIR/skillshub" setup-wizard-skills \
  DATA_ROOT="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}" \
  SCOPE="${MAGICIAN_SETUP_SCOPE:-anonymous/default}"
