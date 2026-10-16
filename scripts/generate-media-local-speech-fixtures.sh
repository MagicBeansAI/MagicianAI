#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="${1:-${repo_root}/data/magician_v2/media_evals/local}"
say_bin="${MAGICIAN_MEDIA_FIXTURE_SAY_BIN:-$(command -v say || true)}"
reuse_existing="${MAGICIAN_MEDIA_REUSE_EXISTING_FIXTURES:-1}"

if [[ -z "${say_bin}" || ! -x "${say_bin}" ]]; then
  printf 'macOS say executable is required to generate synthetic speech fixtures\n' >&2
  exit 1
fi
if ! command -v ffmpeg >/dev/null 2>&1; then
  printf 'ffmpeg is required to normalize media audio fixtures\n' >&2
  exit 1
fi

mkdir -p "${output_dir}"
scratch_dir="$(mktemp -d "${TMPDIR:-/tmp}/magician-speech-fixtures.XXXXXX")"
trap 'rm -rf "${scratch_dir}"' EXIT

generate_fixture() {
  local id="$1"
  local voice="$2"
  local words_per_minute="$3"
  local text="$4"
  local text_path="${scratch_dir}/${id}.txt"
  local raw_path="${scratch_dir}/${id}-raw.aiff"

  printf '%s\n' "${text}" >"${text_path}"
  if [[ "${reuse_existing}" != "0" \
    && -s "${output_dir}/${id}.wav" \
    && -s "${output_dir}/${id}.txt" ]] \
    && cmp -s "${text_path}" "${output_dir}/${id}.txt"; then
    local existing_duration
    existing_duration="$(ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 "${output_dir}/${id}.wav")"
    if [[ -n "${existing_duration}" && "${existing_duration}" != "N/A" ]] \
      && awk -v duration="${existing_duration}" 'BEGIN { exit !(duration >= 0.5) }'; then
      printf 'Reusing existing local fixture %s\n' "${id}"
      return
    fi
  fi
  "${say_bin}" -v "${voice}" -r "${words_per_minute}" -o "${raw_path}" "${text}"
  local duration
  duration="$(ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 "${raw_path}")"
  if [[ -z "${duration}" || "${duration}" == "N/A" ]] \
    || ! awk -v duration="${duration}" 'BEGIN { exit !(duration >= 0.5) }'; then
    printf 'macOS say produced invalid audio for %s (duration=%s)\n' "${id}" "${duration:-unknown}" >&2
    exit 1
  fi
  ffmpeg -hide_banner -loglevel error -nostdin -y \
    -i "${raw_path}" -ar 16000 -ac 1 -c:a pcm_s16le \
    "${output_dir}/${id}.wav"
  cp "${text_path}" "${output_dir}/${id}.txt"
}

generate_fixture \
  "synthetic-clean-en" \
  "Samantha" \
  "180" \
  "Please schedule the design review for Tuesday at ten thirty in the morning and send the agenda to the product team."

generate_fixture \
  "synthetic-indian-en" \
  "Aman" \
  "188" \
  "Please remind me to pay the electricity bill of two thousand four hundred rupees before Friday evening."

generate_fixture \
  "synthetic-hinglish-names-numbers" \
  "Aman" \
  "172" \
  "Riya, kal Ashok ko call karna. Invoice number seven four two one is due on the twenty third of July for twelve thousand five hundred rupees."

speaker_a_path="${output_dir}/synthetic-clean-en.wav"
speaker_b_path="${output_dir}/synthetic-indian-en.wav"
dialogue_path="${output_dir}/synthetic-two-speaker-dialogue.wav"
dialogue_annotation_path="${output_dir}/synthetic-two-speaker-dialogue.json"
silence_seconds="0.4"

speaker_a_seconds="$(ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 "${speaker_a_path}")"
speaker_b_seconds="$(ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 "${speaker_b_path}")"

ffmpeg -hide_banner -loglevel error -nostdin -y \
  -i "${speaker_a_path}" \
  -f lavfi -t "${silence_seconds}" -i "anullsrc=r=16000:cl=mono" \
  -i "${speaker_b_path}" \
  -f lavfi -t "${silence_seconds}" -i "anullsrc=r=16000:cl=mono" \
  -i "${speaker_a_path}" \
  -filter_complex "[0:a][1:a][2:a][3:a][4:a]concat=n=5:v=0:a=1[out]" \
  -map "[out]" -ar 16000 -ac 1 -c:a pcm_s16le "${dialogue_path}"

speaker_a_ms="$(awk -v value="${speaker_a_seconds}" 'BEGIN { printf "%.0f", value * 1000 }')"
speaker_b_ms="$(awk -v value="${speaker_b_seconds}" 'BEGIN { printf "%.0f", value * 1000 }')"
silence_ms="$(awk -v value="${silence_seconds}" 'BEGIN { printf "%.0f", value * 1000 }')"
speaker_b_start_ms="$((speaker_a_ms + silence_ms))"
speaker_b_end_ms="$((speaker_b_start_ms + speaker_b_ms))"
speaker_a_second_start_ms="$((speaker_b_end_ms + silence_ms))"
speaker_a_second_end_ms="$((speaker_a_second_start_ms + speaker_a_ms))"

printf '%s\n' \
  '{' \
  '  "schema_version": 1,' \
  '  "speakers": ["speaker_a", "speaker_b"],' \
  '  "segments": [' \
  "    {\"speaker_id\": \"speaker_a\", \"start_ms\": 0, \"end_ms\": ${speaker_a_ms}}," \
  "    {\"speaker_id\": \"speaker_b\", \"start_ms\": ${speaker_b_start_ms}, \"end_ms\": ${speaker_b_end_ms}}," \
  "    {\"speaker_id\": \"speaker_a\", \"start_ms\": ${speaker_a_second_start_ms}, \"end_ms\": ${speaker_a_second_end_ms}}" \
  '  ]' \
  '}' >"${dialogue_annotation_path}"

printf 'Generated local synthetic speech fixtures in %s\n' "${output_dir}"
