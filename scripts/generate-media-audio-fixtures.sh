#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="${1:-${repo_root}/data/magician_v2/media_evals/generated}"

if ! command -v ffmpeg >/dev/null 2>&1; then
  printf 'ffmpeg is required to generate media audio fixtures\n' >&2
  exit 1
fi

mkdir -p "${output_dir}"
scratch_dir="$(mktemp -d "${TMPDIR:-/tmp}/magician-media-fixtures.XXXXXX")"
trap 'rm -rf "${scratch_dir}"' EXIT

ffmpeg -hide_banner -loglevel error -nostdin -y \
  -f lavfi -i 'anullsrc=r=16000:cl=mono' \
  -t 5 -ar 16000 -ac 1 -c:a pcm_s16le \
  "${scratch_dir}/control-silence-5s.wav"

ffmpeg -hide_banner -loglevel error -nostdin -y \
  -f lavfi -i 'anoisesrc=color=pink:amplitude=0.005:sample_rate=16000:seed=7142026:duration=5' \
  -ar 16000 -ac 1 -c:a pcm_s16le \
  "${scratch_dir}/control-room-noise-5s.wav"

ffmpeg -hide_banner -loglevel error -nostdin -y \
  -f lavfi -i 'sine=frequency=1000:sample_rate=16000:duration=3' \
  -filter:a 'volume=0.1' -ar 16000 -ac 1 -c:a pcm_s16le \
  "${scratch_dir}/control-tone-1khz-3s.wav"

for fixture in "${scratch_dir}"/*.wav; do
  mv "${fixture}" "${output_dir}/$(basename "${fixture}")"
done

printf 'Generated privacy-safe control fixtures in %s\n' "${output_dir}"
