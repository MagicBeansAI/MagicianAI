#!/usr/bin/env bash
# Install Ollama and the compulsory local PPLX memory embedder without choosing
# or downloading a generation model. Desktop handles that separate choice.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MAGICIAN_OLLAMA_SETUP_GENERATION=0 exec bash "$SCRIPT_DIR/setup-ollama-host.sh"
