#!/usr/bin/env bash
# Reuse only the latest completed push CI for this exact main-branch revision,
# and only for one day. Missing API access, malformed data and failed runs all
# fall back to fresh validation; an output never makes an untested revision green.
set -euo pipefail

reusable=false
run_url=
if runs=$(gh api --method GET \
  "repos/$GITHUB_REPOSITORY/actions/workflows/ci.yml/runs" \
  -f "head_sha=$GITHUB_SHA" -f event=push -f status=completed -f per_page=100); then
  if run_url=$(jq -er --arg sha "$GITHUB_SHA" --arg repo "$GITHUB_REPOSITORY" '
    [.workflow_runs[] | select(.head_sha == $sha and .head_branch == "main"
      and .event == "push" and .repository.full_name == $repo)]
    | sort_by(.run_started_at) | last
    | select(.conclusion == "success"
      and (.run_started_at | fromdateiso8601) >= now - 86400)
    | .html_url | select(type == "string" and length > 0)
  ' <<< "$runs"); then
    reusable=true
  fi
fi
printf 'reusable=%s\n' "$reusable" >> "$GITHUB_OUTPUT"
if "$reusable"; then
  printf 'Reusing successful CI for %s: %s\n' "$GITHUB_SHA" "$run_url"
else
  echo 'No recent successful main-branch CI for this exact commit; running all validation.'
fi
