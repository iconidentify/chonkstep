#!/usr/bin/env bash
# Reuse only the latest successful push CI for this exact main-branch revision.
# Validation expires after one day; the built packages last until artifact expiry.
# A tag arriving during main CI waits for that run instead of duplicating it.
# Packages come only from that successful run and must include both architectures.
set -euo pipefail

reusable=false
package_run_id=
package_artifact_ids=
run=null
# These are jq variables supplied with --arg below, not shell expansions.
# shellcheck disable=SC2016
select_run='select(.head_sha == $sha and .head_branch == "main"
  and .event == "push" and .repository.full_name == $repo)'
if runs=$(gh api --method GET \
  "repos/$GITHUB_REPOSITORY/actions/workflows/ci.yml/runs" \
  -f "head_sha=$GITHUB_SHA" -f event=push -f per_page=100); then
  run=$(jq -ce --arg sha "$GITHUB_SHA" --arg repo "$GITHUB_REPOSITORY" \
    "[.workflow_runs[] | $select_run] | sort_by(.run_started_at, .id) | last" \
    <<< "$runs") || run=null
fi

if run_id=$(jq -er 'select(.status != "completed") | .id
  | select(type == "number" and . > 0)' <<< "$run"); then
  echo "Waiting for main CI $run_id to finish building and validating $GITHUB_SHA."
  # A timeout/API failure must not start competing builds while the original
  # run might still be active. Completed failures fall back to fresh CI below.
  timeout 35m gh run watch "$run_id" --repo "$GITHUB_REPOSITORY" --interval 15
  run=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run_id")
fi

if run_id=$(jq -er --arg sha "$GITHUB_SHA" --arg repo "$GITHUB_REPOSITORY" \
  "$select_run"' | select(.status == "completed" and .conclusion == "success")
  | .id | select(type == "number" and . > 0)' <<< "$run"); then
  if jq -e '(.run_started_at | fromdateiso8601) >= now - 86400' <<< "$run" >/dev/null; then
    reusable=true
  fi
  if artifacts=$(gh api --method GET \
    "repos/$GITHUB_REPOSITORY/actions/runs/$run_id/artifacts" -f per_page=100); then
    if package_artifact_ids=$(jq -er --arg sha "$GITHUB_SHA" --argjson run "$run_id" '
      [.artifacts[] | select(.name == "chonkstep-package-x86_64"
        or .name == "chonkstep-package-aarch64")]
      | select(length == 2 and (map(.name) | unique | length) == 2)
      | select(all(.[]; .expired == false and (.expires_at | fromdateiso8601) > now
        and .workflow_run.id == $run and .workflow_run.head_sha == $sha
        and .workflow_run.head_branch == "main"
        and .workflow_run.repository_id == .workflow_run.head_repository_id
        and (.id | type == "number" and . > 0)))
      | sort_by(.name) | map(.id | tostring) | join(",")
    ' <<< "$artifacts"); then
      package_run_id=$run_id
    else
      package_artifact_ids=
    fi
  fi
fi

printf 'reusable=%s\npackage_run_id=%s\npackage_artifact_ids=%s\n' \
  "$reusable" "$package_run_id" "$package_artifact_ids" >> "$GITHUB_OUTPUT"
if "$reusable"; then
  printf 'Reusing successful CI for %s: https://github.com/%s/actions/runs/%s\n' \
    "$GITHUB_SHA" "$GITHUB_REPOSITORY" "$run_id"
else
  echo 'No recent successful main-branch CI for this exact commit; running all validation.'
fi
if [[ -n $package_run_id ]]; then
  echo "Promoting both native package artifacts from run $package_run_id; no recompilation."
else
  echo 'No complete reusable package pair; building and verifying both architectures.'
fi
