#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/setup-branch-protection.sh
#
# One-shot, idempotent: enforce the two-branch policy on CZython/knowledgebase.
#   dev  — PR merges need: CI checks green + a CLEAN REBASE (no human review)
#            The rebase gate is the `rebase-check` job in
#            .github/workflows/auto-merge-dev.yml, which is REQUIRED on dev.
#            A dirty/conflicting rebase therefore blocks the PR for everyone.
#   main — PR merges need: CI green + code-owner sign-off (@CZython)
#
# Order matters and is chosen so the rules never block their own setup:
#   0. preflight (token + repo reach)
#   1. re-affirm CODEOWNERS + the ci trigger files (already in tree)
#   2. commit any drift
#   3. push the committed state over HTTPS  (SSH push needs a separate setup)
#   4. branch-protection for `main`
#   5. branch-protection for `dev`
#   6. summary
#
# USAGE
#   # a PAT with `repo` scope, or fine-grained: Contents: Read+Write,
#   # Metadata: Read (that is enough to set branch protection).
#   export GITHUB_TOKEN=ghp_...
#   scripts/setup-branch-protection.sh [--push-dev | --push-main]
#
#     --push-dev    (default) push HEAD -> origin/dev  (then PR dev -> main)
#     --push-main   push HEAD -> dev AND origin/main
#
# NOTE on "required checks": GitHub matches required status checks by *job*
# name, not workflow name (and the PUT field is `contexts`, not the
# `required_status_check_names` the docs sometimes mention). `ci.yml` defines
# three jobs — `build-and-test`, `linux-musl`, `verus-verify` — required on
# BOTH branches (so a red Verus proof or a bad musl artifact can't slip in).
# `dev` additionally requires `rebase-check` (from auto-merge-dev.yml): the
# auto-merge's verdict job, which goes RED when the rebase is dirty — that is
# what hard-blocks a conflicting PR (GitHub refuses to merge while a required
# check is red, even for admins with enforce_admins on).
# ---------------------------------------------------------------------------
set -euo pipefail

OWNER="CZython"
REPO="knowledgebase"
API="https://api.github.com"
CODEOWNER="@CZython"
CI_JOB_NAMES_JSON='["build-and-test", "linux-musl", "verus-verify"]'
DEV_CONTEXTS_JSON='["build-and-test", "linux-musl", "verus-verify", "rebase-check"]'

# ---------- 0. preflight ----------
if [[ -z "${GITHUB_TOKEN:-}" ]]; then
    echo "error: set GITHUB_TOKEN (a PAT with repo / Contents:Read+Write)" >&2
    exit 2
fi

H=( -H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/vnd.github+json" )
j() { curl -sS "${H[@]}" "$@"; }

# Confirm reach + that the token can see the repo.
me=$(j "$API/user" | python3 -c 'import sys,json;print(json.load(sys.stdin)["login"])')
echo "authenticated as: $me"
j -o /dev/null -w "repo reach: http %{http_code}\n" "$API/repos/$OWNER/$REPO"

# ---------- 1. re-affirm in-tree files ----------
# 1a. CODEOWNERS — makes `require_code_owner_reviews` mean *you* on every
#     path (the "maintainer sign-off" half of the main gate).
if [[ -f CODEOWNERS ]]; then
    echo "CODEOWNERS already present in tree — leaving as-is"
else
    echo "note: CODEOWNERS missing — writing a minimal one (**  $CODEOWNER)"
    printf '**          %s\n' "$CODEOWNER" > CODEOWNERS
fi

# 1b. ci.yml — must trigger push on `dev` too, otherwise the `dev`
# required-check has no run to pass/fail against.
ci_file=".github/workflows/ci.yml"
if grep -qE 'branches:\s*\[[^]]*\bmain\b[^]]*\bdev\b[^]]*\]' "$ci_file"; then
    echo "ci.yml already triggers on dev — OK"
else
    echo "error: $ci_file push.branches must include both main and dev." >&2
    echo "        (edit it to: branches: [ main, dev ])" >&2
    exit 1
fi

# 1c. auto-merge-dev.yml — the `rebase-check` job is a REQUIRED check on dev,
# so the workflow that produces it must exist (on dev) for the gate to work.
am_file=".github/workflows/auto-merge-dev.yml"
if [[ -f "$am_file" ]]; then
    grep -q '^  rebase-check:' "$am_file" \
      && echo "auto-merge-dev.yml defines rebase-check — OK" \
      || { echo "error: $am_file must define a `rebase-check` job (it is required on dev)." >&2; exit 1; }
else
    echo "error: $am_file missing — the `rebase-check` required check has no producer." >&2
    exit 1
fi

# ---------- 2. commit any drift ----------
staged=0
for f in CODEOWNERS "$ci_file" "$am_file" scripts/setup-branch-protection.sh; do
    if [[ -f "$f" ]]; then
        git add -A "$f"
        if ! git diff --cached --quiet -- "$f" 2>/dev/null; then
            staged=1
        fi
    fi
done
if [[ "$staged" == 1 ]]; then
    git commit -q -m "branch protection: CODEOWNERS + ci triggers on dev + setup script"
    echo "committed: $(git log --oneline -1)"
else
    echo "commit step: no drift (already clean)"
fi

# ---------- 3. push over HTTPS ----------
# SSH push is not configured in this environment, so push over HTTPS using
# the token in the URL (built per-invocation; never persisted to remotes).
AUTH_URL="https://x-access-token:${GITHUB_TOKEN}@github.com/${OWNER}/${REPO}.git"
push_to_branch() {
    local br="$1"
    echo "pushing HEAD -> $br"
    git push "$AUTH_URL" "HEAD:refs/heads/$br"
}

case "${1:-}" in
    --push-main)  push_to_branch dev; push_to_branch main ;;
    ''|--push-dev) push_to_branch dev ;;
    -h|--help)    grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)            echo "usage: $0 [--push-dev | --push-main]" >&2; exit 2 ;;
esac

# ---------- 4. branch protection: main ----------
#  - required review:  1 approval, code-owner sign-off ON
#  - required checks: the two CI jobs (strict: branch must be up to date)
#  - admins are bound too (enforce_admins)
main_rule='
{
  "protected": true,
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews_on_push": true,
    "require_code_owner_reviews": true
  },
  "required_status_checks": {
    "strict": true,
    "contexts": '"$CI_JOB_NAMES_JSON"'
  },
  "restrictions": null,
  "required_pull_request_reviews_owners": null,
  "enforce_admins": true,
  "required_linear_history": false,
  "allow_force_pushes": false,
  "allow_deletions": false
}'
main_http=$(j -o /tmp/kb_main_prot.json -w '%{http_code}' \
  -X PUT -H "Content-Type: application/json" \
  --data "$main_rule" \
  "$API/repos/$OWNER/$REPO/branches/main/protection")
main_body=$(cat /tmp/kb_main_prot.json)
if [[ "$main_http" =~ ^(200|201)$ ]]; then
    echo "main protection: OK (http $main_http)"
else
    echo "main protection: FAILED (http $main_http)" >&2; echo "$main_body" >&2; exit 1
fi

# ---------- 5. branch protection: dev ----------
#  - required review:  none (CI is the quality gate for dev)
#  - required checks: same two CI jobs (strict: false — dev is fast-moving)
#  - admins are bound too
dev_rule='
{
  "protected": true,
  "required_pull_request_reviews": {
    "required_approving_review_count": 0,
    "dismiss_stale_reviews_on_push": false,
    "require_code_owner_reviews": false
  },
  "required_status_checks": {
    "strict": false,
    "contexts": '"$DEV_CONTEXTS_JSON"'
  },
  "restrictions": null,
  "enforce_admins": true,
  "required_linear_history": false,
  "allow_force_pushes": false,
  "allow_deletions": false
}'
dev_http=$(j -o /tmp/kb_dev_prot.json -w '%{http_code}' \
  -X PUT -H "Content-Type: application/json" \
  --data "$dev_rule" \
  "$API/repos/$OWNER/$REPO/branches/dev/protection")
dev_body=$(cat /tmp/kb_dev_prot.json)
if [[ "$dev_http" =~ ^(200|201)$ ]]; then
    echo "dev protection: OK (http $dev_http)"
else
    echo "dev protection: FAILED (http $dev_http)" >&2; echo "$dev_body" >&2; exit 1
fi

# ---------- 6. summary ----------
echo
echo "OK. Two-branch policy is now enforced on $OWNER/$REPO:"
echo
echo "  main  –  required before merge:"
echo "          (a) all 3 CI jobs green: build-and-test, linux-musl, verus-verify"
echo "          (b) 1 approving review from a code owner ($CODEOWNER)"
echo "          –  force-push / deletes: disallowed, admins bound"
echo
echo "  dev   –  required before merge:"
echo "          (a) all 3 CI jobs green: build-and-test, linux-musl, verus-verify"
echo "          (b) rebase-check green (a CLEAN rebase) — auto-merge-dev.yml"
echo "          –  no human review, no code-owner requirement"
echo "          –  auto-merges by REBASE once all the above pass"
echo
echo "  CI trigger – .github/workflows/ci.yml runs on push [ main, dev ] + PR"
echo "  CODEOWNERS – ** @CZython  (root pattern covers the whole tree)"
echo
echo "  Workflow you asked for:"
echo "    * dev:  push a feature branch -> PR into dev -> CI -> merge (no review)"
echo "    * main: push dev (or a PR) -> PR into main -> CI -> YOU approve -> merge"
rm -f /tmp/kb_main_prot.json /tmp/kb_dev_prot.json
