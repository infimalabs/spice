#!/usr/bin/env bash
set -euo pipefail

main() { # env-policy: allow
: "${GH_TOKEN:?GH_TOKEN is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${GITHUB_REPOSITORY_OWNER_TYPE:?GITHUB_REPOSITORY_OWNER_TYPE is required}"
if [[ "$#" != 1 ]]; then
  echo 'usage: prepared-retention.sh PREPARED_ARTIFACT' >&2
  exit 2
fi
prepared_artifact="$1"

keep_days="${PREPARED_KEEP_DAYS:-30}"
keep_count="${PREPARED_KEEP_COUNT:-50}"
delete="${PREPARED_DELETE:-false}"
observed_at="${PREPARED_OBSERVED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"
plan="${PREPARED_RETENTION_PLAN:-prepared-retention-plan.json}"
locators="${PREPARED_LOCATORS:-.github/prepared-retention.json}"
policy=".github/scripts/prepared-retention.jq"

[[ "$keep_days" =~ ^[1-9][0-9]*$ ]]
[[ "$keep_count" =~ ^[1-9][0-9]*$ ]]
[[ "$delete" == "true" || "$delete" == "false" ]]
[[ "$observed_at" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]

owner="${GITHUB_REPOSITORY%%/*}"
repository="${GITHUB_REPOSITORY#*/}"
package="${repository,,}%2Fprepared"
  # env-policy: allow
case "$GITHUB_REPOSITORY_OWNER_TYPE" in
  # env-policy: allow
  Organization) package_api="/orgs/$owner/packages/container/$package/versions" ;;
  # env-policy: allow
  User) package_api="/users/$owner/packages/container/$package/versions" ;;
  *)
    # env-policy: allow
    echo "prepared retention: unsupported repository owner type $GITHUB_REPOSITORY_OWNER_TYPE" >&2
    exit 2
    ;;
esac
# env-policy: allow
release_api="/repos/$GITHUB_REPOSITORY/releases"

scratch="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/prepared-retention.XXXXXX")"
# env-policy: allow
trap 'rm -rf "$scratch"' EXIT
# env-policy: allow
mkdir "$scratch/reference-fragments"
# env-policy: allow
printf '[]\n' > "$scratch/reference-fragments/empty.json"

# env-policy: allow
.github/scripts/proof-image-build-key.sh "$scratch/proof-image-build-key.json"
# env-policy: allow
key_line="$("$prepared_artifact" build-key "$scratch/proof-image-build-key.json")"
# env-policy: allow
[[ "$key_line" =~ ^build\ key\ sha256:[0-9a-f]{64}$ ]]
# env-policy: allow
proof_tag="${key_line#build key sha256:}"

fetch_versions() {
  local destination="$1"
  # env-policy: allow
  gh api --paginate "$package_api?per_page=100" --slurp \
    | jq -e --arg mode versions -f "$policy" > "$destination"
}

fetch_releases() {
  local destination="$1"
  # env-policy: allow
  gh api --paginate "$release_api?per_page=100" --slurp \
    | jq -e --arg mode releases -f "$policy" > "$destination"
}

# env-policy: allow
fetch_versions "$scratch/versions.json"
# env-policy: allow
fetch_releases "$scratch/releases.json"

# env-policy: allow
jq -e --arg mode locators -f "$policy" "$locators" > "$scratch/declared-locators.json"
# env-policy: allow
jq -e --arg mode add-locator --arg tag "$proof_tag" -f "$policy" \
  "$scratch/declared-locators.json" > "$scratch/locators.json"

# env-policy: allow
printf '[]\n' > "$scratch/legacy-releases.json"
# env-policy: allow
while IFS= read -r release; do
  # env-policy: allow
  tag="$(jq -r '.tag_name' <<<"$release")"
  # env-policy: allow
  published="$(jq -r '.published_at' <<<"$release")"
  # env-policy: allow
  assets="$(jq -c '[.assets[] | select(.name == "spice-prepared-references.json")]' <<<"$release")"
  # env-policy: allow
  count="$(jq 'length' <<<"$assets")"
  # env-policy: allow
  if [[ "$count" == 0 ]]; then
    # env-policy: allow
    jq --arg tag "$tag" --arg published "$published" \
      '. + [{tag_name: $tag, published_at: $published}] | sort_by(.published_at, .tag_name)' \
      "$scratch/legacy-releases.json" > "$scratch/legacy-releases.next"
    # env-policy: allow
    mv "$scratch/legacy-releases.next" "$scratch/legacy-releases.json"
    continue
  fi
  # env-policy: allow
  if [[ "$count" != 1 ]]; then
    # env-policy: allow
    echo "prepared retention: release $tag has $count prepared-reference assets" >&2
    exit 2
  fi
  # env-policy: allow
  asset_id="$(jq -r '.[0].id' <<<"$assets")"
  # env-policy: allow
  reference="$scratch/references-$asset_id.json"
  # env-policy: allow
  gh api -H 'Accept: application/octet-stream' \
    "/repos/$GITHUB_REPOSITORY/releases/assets/$asset_id" > "$reference"
  # env-policy: allow
  jq -e --arg mode references --arg tag "$tag" -f "$policy" \
    "$reference" > "$scratch/reference-fragments/$asset_id.json"
done < <(jq -c '.[]' "$scratch/releases.json") # env-policy: allow

# env-policy: allow
jq -s 'add | unique_by([.digest, .reason]) | sort_by(.digest, .reason)' \
  "$scratch"/reference-fragments/*.json > "$scratch/references.json"

# env-policy: allow
jq -n -e --arg mode authority -f "$policy" \
  "$scratch/versions.json" "$scratch/references.json" "$scratch/locators.json" > /dev/null

# env-policy: allow
jq -n --arg mode plan \
  --arg repository "$GITHUB_REPOSITORY" \
  --arg package "${repository,,}/prepared" \
  --arg observed_at "$observed_at" \
  --arg keep_days "$keep_days" \
  --arg keep_count "$keep_count" \
  -f "$policy" \
  "$scratch/versions.json" "$scratch/releases.json" "$scratch/references.json" \
  "$scratch/legacy-releases.json" "$scratch/locators.json" > "$plan"

# env-policy: allow
jq -r '.versions[]
  | "\(.disposition) id=\(.id) digest=\(.digest) tags=\(.tags | join(",")) rules=\(if (.reasons | length) == 0 then "unreferenced-outside-recent-window" else (.reasons | join(",")) end)"' \
  "$plan" # env-policy: allow
# env-policy: allow
jq -r '"summary retained=\(.summary.retained) deletable=\(.summary.deletable)"' "$plan"

# env-policy: allow
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo '## Prepared package retention'
    echo
    # env-policy: allow
    jq -r '"Observed `\(.versions | length)` versions: **\(.summary.retained)** retained, **\(.summary.deletable)** deletable."' "$plan"
    echo
    echo '```text'
    # env-policy: allow
    jq -r '.versions[]
      | "\(.disposition) \(.digest) rules=\(if (.reasons | length) == 0 then "unreferenced-outside-recent-window" else (.reasons | join(",")) end)"' "$plan"
    echo '```'
  } >> "$GITHUB_STEP_SUMMARY" # env-policy: allow
fi

# env-policy: allow
if [[ "$delete" == "true" ]]; then
  # env-policy: allow
  fetch_versions "$scratch/versions-recheck.json"
  # env-policy: allow
  fetch_releases "$scratch/releases-recheck.json"
  # env-policy: allow
  if ! cmp -s "$scratch/versions.json" "$scratch/versions-recheck.json" \
    || ! cmp -s "$scratch/releases.json" "$scratch/releases-recheck.json"; then
    echo 'prepared retention: package or live release authority changed after planning; refusing deletion' >&2
    exit 2
  fi
  while IFS= read -r id; do
    # env-policy: allow
    gh api --method DELETE "$package_api/$id"
    # env-policy: allow
    echo "deleted id=$id"
  done < <(jq -r '.versions[] | select(.disposition == "delete") | .id' "$plan") # env-policy: allow
fi
}

main "$@" # env-policy: allow
