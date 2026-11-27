def versions:
  [ .[][] ]
  | sort_by(.id)
  | if type != "array"
    or (all(.[ ];
        (.id | type == "number")
        and (.name | type == "string" and test("^sha256:[0-9a-f]{64}$"))
        and (.created_at | type == "string" and (try fromdateiso8601 catch null) != null)
        and (.updated_at | type == "string" and (try fromdateiso8601 catch null) != null)
        and (.metadata.container.tags | type == "array")
        and (all(.metadata.container.tags[]; type == "string"))) | not)
    or ([.[].id] | length) != ([.[].id] | unique | length)
    or ([.[].name] | length) != ([.[].name] | unique | length)
    then error("package version inventory is incomplete or malformed")
    else . end;

def releases:
  [ .[][]
    | select(.draft == false)
    | {
        id,
        tag_name,
        published_at,
        assets: [ .assets[] | {id, name, size, updated_at} ] | sort_by(.id)
      }
  ]
  | sort_by(.id)
  | if type != "array"
    or (all(.[ ];
        (.id | type == "number")
        and (.tag_name | type == "string" and length > 0)
        and (.published_at | type == "string" and (try fromdateiso8601 catch null) != null)
        and (.assets | type == "array")
        and (all(.assets[];
            (.id | type == "number")
            and (.name | type == "string")
            and (.size | type == "number")
            and (.updated_at | type == "string" and (try fromdateiso8601 catch null) != null)))) | not)
    then error("release inventory is incomplete or malformed")
    else . end;

def locators:
  if keys != ["manifests", "schema", "tags"]
    or .schema != 1
    or (.manifests | type != "array")
    or (.tags | type != "array")
    or (all(.manifests[]; type == "string" and test("^sha256:[0-9a-f]{64}$")) | not)
    or (all(.tags[]; type == "string" and test("^[0-9a-f]{64}$")) | not)
  then error("current prepared locator file is malformed")
  else {
    schema,
    manifests: (.manifests | unique | sort),
    tags: (.tags | unique | sort)
  } end;

def add_locator:
  if ($ARGS.named.tag | test("^[0-9a-f]{64}$")) | not
  then error("derived prepared locator is malformed")
  else .tags = ((.tags + [$ARGS.named.tag]) | unique | sort) end;

def references:
  if keys != ["manifests", "schema"]
    or .schema != 1
    or (.manifests | type != "array" or length == 0)
    or (all(.manifests[]; type == "string" and test("^sha256:[0-9a-f]{64}$")) | not)
  then error("release prepared-reference asset is malformed")
  else [ .manifests[] | {digest: ., reason: ("live-release:" + $ARGS.named.tag)} ] end;

def authority:
  input as $versions
  | input as $references
  | input as $locators
  | ($versions | [.[].name]) as $digests
  | ($versions | [.[].metadata.container.tags[]]) as $tags
  | if ($references | all(.[]; .digest as $digest | $digests | index($digest) != null) | not)
    or ($locators.manifests | all(.[]; . as $digest | $digests | index($digest) != null) | not)
    or ($locators.tags | all(.[]; . as $tag | $tags | index($tag) != null) | not)
    then error("a live release or current locator names an absent prepared version")
    else true end;

def plan:
  input as $versions
  | input as $releases
  | input as $references
  | input as $legacy
  | input as $locators
  | ($ARGS.named.observed_at | fromdateiso8601) as $observed
  | ($ARGS.named.keep_days | tonumber) as $keep_days
  | ($ARGS.named.keep_count | tonumber) as $keep_count
  | ($observed - ($keep_days * 86400)) as $cutoff
  | ($versions
      | sort_by(.updated_at, .id)
      | reverse
      | to_entries
      | map({key: (.value.id | tostring), value: .key})
      | from_entries) as $ranks
  | [ $versions[] as $version
      | ([ $references[]
            | select(.digest == $version.name)
            | .reason ]
        + [ $locators.manifests[]
            | select(. == $version.name)
            | "current-locator:manifest" ]
        + [ $locators.tags[] as $wanted
            | $version.metadata.container.tags[]
            | select(. == $wanted)
            | ("current-locator:tag:" + .) ]
        + [ $legacy[]
            | select(($version.created_at | fromdateiso8601) <= (.published_at | fromdateiso8601))
            | ("legacy-release-closure:" + .tag_name) ]
        + [ if ($version.updated_at | fromdateiso8601) >= $cutoff
            then "recent-age" else empty end ]
        + [ if $ranks[($version.id | tostring)] < $keep_count
            then "recent-count" else empty end ]
        | unique | sort) as $reasons
      | {
          id: $version.id,
          digest: $version.name,
          created_at: $version.created_at,
          updated_at: $version.updated_at,
          tags: ($version.metadata.container.tags | sort),
          disposition: (if ($reasons | length) == 0 then "delete" else "retain" end),
          reasons: $reasons
        }
    ] as $classified
  | {
      schema: 1,
      repository: $ARGS.named.repository,
      package: $ARGS.named.package,
      observed_at: $ARGS.named.observed_at,
      policy: {keep_days: $keep_days, keep_count: $keep_count},
      authority: {
        live_releases: $releases,
        release_references: $references,
        legacy_releases: $legacy,
        current_locators: $locators
      },
      versions: $classified,
      summary: {
        retained: ([$classified[] | select(.disposition == "retain")] | length),
        deletable: ([$classified[] | select(.disposition == "delete")] | length)
      }
    };

if $ARGS.named.mode == "versions" then versions
elif $ARGS.named.mode == "releases" then releases
elif $ARGS.named.mode == "locators" then locators
elif $ARGS.named.mode == "add-locator" then add_locator
elif $ARGS.named.mode == "references" then references
elif $ARGS.named.mode == "authority" then authority
elif $ARGS.named.mode == "plan" then plan
else error("unknown prepared retention policy mode")
end
