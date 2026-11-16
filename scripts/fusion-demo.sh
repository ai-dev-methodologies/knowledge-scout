#!/usr/bin/env bash
# Demo of the fusion ranker (docs/FUSION.md): build the index under ksf: (the fusion namespace; ks: and ks-eval-* are refused)
# on the Redis ks uses, then run five searches with
# the embedding switch off and on/auto. Run with an explicit interpreter:
#   bash scripts/fusion-demo.sh <data dir> <alias dictionary> <model json> [<card vector file>] [<long summary dir>]
# Environment: KS_BIN (default target/release/ks), KS_REDIS_URL / KS_REDIS_* (as for every ks command; values never printed),
#   KS_EMBED_URL (http://127.0.0.1:<port>, a local bge-m3 llama-server: with it the second run uses the embedding path;
#   without it switch auto falls back to the lexical path and says so), KS_FUSION_SKIP_BUILD=1 (reuse the ksf: index).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ks="${KS_BIN:-$here/../target/release/ks}"
[ $# -ge 3 ] || { echo "usage: bash scripts/fusion-demo.sh <data dir> <aliases.json> <fusion-model.json> [<vectors.bin>] [<long summary dir>]" >&2; exit 2; }
data="$1"; aliases="$2"; model="$3"; vectors="${4:-}"; long="${5:-}"

if [ "${KS_FUSION_SKIP_BUILD:-}" != 1 ]; then
  args=(fusion build --data "$data" --aliases "$aliases" --model "$model" --prefix ksf:)
  [ -n "$vectors" ] && args+=(--vectors "$vectors")
  [ -n "$long" ] && args+=(--long-dir "$long")
  echo "== build under ksf:"
  "$ks" "${args[@]}" | jq -c .
fi

queries=(
  "LLM 심판으로 두 랭커를 비교할 때 보여주는 순서 때문에 생기는 편향을 줄이는 방법"
  "에이전트 메모리 관리 — context window 압축 전략"
  "prompt injection defenses for tool-calling agents"
  "코드 리뷰 자동화 파이프라인을 CI/CD에 붙이기"
  "distributed tracing for LLM applications with OpenTelemetry"
)
second=(--switch auto)
[ -n "${KS_EMBED_URL:-}" ] && second+=(--embed-url "$KS_EMBED_URL")
for q in "${queries[@]}"; do
  echo
  echo "== $q"
  for sw in off second; do
    if [ "$sw" = off ]; then extra=(--switch off); else extra=("${second[@]}"); fi
    "$ks" fusion search --query "$q" --limit 3 "${extra[@]}" \
      | jq -r '"  switch \(.switch): path \(.path) (\(.reason)), \(.candidates) candidates, \(.ms.total | floor) ms\n" + ([.results[] | "    \(.rank). \(.id)  \(.title)"] | join("\n"))'
  done
done
