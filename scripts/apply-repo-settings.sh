#!/usr/bin/env bash
# Apply the repository settings described in docs/REPO_SETTINGS.md with the
# GitHub API. Idempotent: it reads the current state, prints every difference,
# and only writes what differs. Run it again at any time to detect drift.
#
# Usage:
#   scripts/apply-repo-settings.sh [--dry-run] [--skip-copilot] OWNER/REPO
#
#   --dry-run       show what would change; make no changes (GET requests only)
#   --skip-copilot  do not manage the Copilot code review ruleset
#
# Requires: gh (authenticated as a repository admin), jq.
set -euo pipefail

dry_run=0
skip_copilot=0
repo=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) dry_run=1 ;;
        --skip-copilot) skip_copilot=1 ;;
        -h|--help) sed -n '2,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "apply-repo-settings: unknown option $arg" >&2; exit 2 ;;
        *) repo="$arg" ;;
    esac
done
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
    || { echo "usage: $0 [--dry-run] [--skip-copilot] OWNER/REPO" >&2; exit 2; }
for tool in gh jq; do
    command -v "$tool" >/dev/null || { echo "apply-repo-settings: $tool is required" >&2; exit 2; }
done

# Required status checks: the exact job names of the workflows that must pass
# before anything merges into main. GitHub Actions' app id pins the source so
# a commit status from another integration cannot satisfy them.
GITHUB_ACTIONS_APP_ID=15368
common_checks=(
    "rustfmt"
    "clippy"
    "Tests"
    "Shell lint & installer sanity"
    "Firewall namespace test"
    "Kill switch e2e"
    "cargo audit (RUSTSEC advisories)"
    "Version consistency"
    "MSRV"
    "Fuzz crate check"
    "Latest compatible dependencies"
)
case "${repo#*/}" in
    wireguard-tui) checks=("${common_checks[@]}" "Release build & CLI smoke") ;;
    wireguard-gui) checks=("${common_checks[@]}" "Release build, CLI & GUI smoke") ;;
    *) echo "apply-repo-settings: no check list for ${repo#*/}" >&2; exit 2 ;;
esac

changes=0
say() { printf '%s\n' "$*"; }
change() { changes=$((changes + 1)); printf 'change  %s\n' "$*"; }
same() { printf 'ok      %s\n' "$*"; }

api() { gh api -H "Accept: application/vnd.github+json" -H "X-GitHub-Api-Version: 2022-11-28" "$@"; }
write_api() {
    # write_api METHOD PATH [JSON]
    local method="$1" path="$2" body="${3:-}"
    if [ "$dry_run" = 1 ]; then
        say "        (dry run) $method $path"
        return 0
    fi
    if [ -n "$body" ]; then
        printf '%s' "$body" | api -X "$method" "$path" --input - >/dev/null
    else
        api -X "$method" "$path" >/dev/null
    fi
}

say "Repository settings for $repo$([ "$dry_run" = 1 ] && echo ' (dry run)')"
current="$(api "repos/$repo")"
[ "$(jq -r '.permissions.admin' <<<"$current")" = true ] \
    || { echo "apply-repo-settings: admin access to $repo is required" >&2; exit 1; }
default_branch="$(jq -r .default_branch <<<"$current")"

# ------------------------------------------------------------ repository flags
desired_repo='{"allow_auto_merge":true,"delete_branch_on_merge":true,"allow_update_branch":true}'
patch='{}'
for key in $(jq -r 'keys[]' <<<"$desired_repo"); do
    want="$(jq -c --arg k "$key" '.[$k]' <<<"$desired_repo")"
    have="$(jq -c --arg k "$key" '.[$k]' <<<"$current")"
    if [ "$want" = "$have" ]; then
        same "repo $key = $have"
    else
        change "repo $key: $have -> $want"
        patch="$(jq -c --arg k "$key" --argjson v "$want" '.[$k] = $v' <<<"$patch")"
    fi
done
[ "$patch" = '{}' ] || write_api PATCH "repos/$repo" "$patch"

# ------------------------------------------------------------ Actions policy
wf="$(api "repos/$repo/actions/permissions/workflow")"
if [ "$(jq -r '.default_workflow_permissions + " " + (.can_approve_pull_request_reviews|tostring)' <<<"$wf")" = "read false" ]; then
    same "Actions default token: read-only, cannot approve pull requests"
else
    change "Actions default token: $(jq -c . <<<"$wf") -> read-only, cannot approve"
    write_api PUT "repos/$repo/actions/permissions/workflow" \
        '{"default_workflow_permissions":"read","can_approve_pull_request_reviews":false}'
fi
ap="$(api "repos/$repo/actions/permissions")"
if [ "$(jq -r 'has("sha_pinning_required")' <<<"$ap")" != true ]; then
    say "skip    Actions SHA pinning requirement (not offered by the API for this account)"
elif [ "$(jq -r .sha_pinning_required <<<"$ap")" = true ]; then
    same "Actions must be pinned to a full commit SHA"
else
    change "Actions SHA pinning required: false -> true"
    write_api PUT "repos/$repo/actions/permissions" \
        "$(jq -c '{enabled: .enabled, allowed_actions: .allowed_actions, sha_pinning_required: true}' <<<"$ap")"
fi

# ------------------------------------------------------------ Dependabot security
if api "repos/$repo/vulnerability-alerts" >/dev/null 2>&1; then
    same "Dependabot alerts enabled"
else
    change "Dependabot alerts: disabled -> enabled"
    write_api PUT "repos/$repo/vulnerability-alerts"
fi
if [ "$(api "repos/$repo/automated-security-fixes" 2>/dev/null | jq -r '.enabled // false')" = true ]; then
    same "Dependabot security updates enabled"
else
    change "Dependabot security updates: disabled -> enabled"
    write_api PUT "repos/$repo/automated-security-fixes"
fi

# ------------------------------------------------------------ release environment
# The signing job runs in the `release` environment, which only deployments
# from v* tags may use. Store MINISIGN_KEY as a secret of this environment.
env_json="$(api "repos/$repo/environments/release" 2>/dev/null || echo '{}')"
if [ "$(jq -r '.deployment_branch_policy.custom_branch_policies // false' <<<"$env_json")" = true ]; then
    same "environment release: custom deployment policy"
else
    change "environment release: create/restrict to custom deployment refs"
    write_api PUT "repos/$repo/environments/release" \
        '{"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}'
fi
policies="$(api "repos/$repo/environments/release/deployment-branch-policies" 2>/dev/null || echo '{"branch_policies":[]}')"
if jq -e '.branch_policies[]? | select(.name == "v*" and .type == "tag")' <<<"$policies" >/dev/null; then
    same "environment release: only tags v* may deploy"
else
    change "environment release: allow deployments from tags v*"
    write_api POST "repos/$repo/environments/release/deployment-branch-policies" '{"name":"v*","type":"tag"}'
fi
extra_policies="$(jq -r '[.branch_policies[]? | select(.name != "v*" or .type != "tag") | "\(.type):\(.name)"] | join(", ")' <<<"$policies")"
[ -z "$extra_policies" ] || say "warn    environment release also allows: $extra_policies (remove unless intended)"
if api "repos/$repo/environments/release/secrets/MINISIGN_KEY" >/dev/null 2>&1; then
    same "environment release holds MINISIGN_KEY"
else
    say "todo    move MINISIGN_KEY into the release environment:"
    say "          gh secret set MINISIGN_KEY --env release --repo $repo < minisign.key"
    say "          gh secret delete MINISIGN_KEY --repo $repo"
fi

# ------------------------------------------------------------ rulesets
existing_rulesets="$(api "repos/$repo/rulesets?includes_parents=false" --paginate | jq -s 'add // []')"

# Project a ruleset onto the fields this script manages (taken from the
# desired definition), in a stable order, so fields GitHub adds later do not
# show up as drift. Rules present on GitHub but not desired are listed too.
project() {
    # project DESIRED_JSON < RULESET_JSON
    jq -S --argjson d "$1" '
        def sorted_checks: if .required_status_checks then .required_status_checks |= sort_by(.context) else . end;
        . as $have | {
            name, target, enforcement,
            conditions: (.conditions // {}),
            bypass_actors: ([.bypass_actors[]? | {actor_id, actor_type, bypass_mode}] | sort_by(.actor_type, .actor_id)),
            rules: [$d.rules[] as $want
                | ($have.rules // [] | map(select(.type == $want.type)) | first) as $got
                | if $got == null then {type: $want.type, missing: true}
                  else {type: $got.type}
                    + (if $want.parameters then {parameters: ($got.parameters // {}
                        | with_entries(select(.key as $k | $want.parameters | has($k)))
                        | sorted_checks)} else {} end)
                  end],
            unmanaged_rules: [($have.rules // [])[] | .type | select(. as $t | [$d.rules[].type] | index($t) | not)]
        }'
}

apply_ruleset() {
    # apply_ruleset JSON [optional]
    local desired="$1" optional="${2:-}" name id have want
    name="$(jq -r .name <<<"$desired")"
    id="$(jq -r --arg n "$name" '.[] | select(.name == $n) | .id' <<<"$existing_rulesets" | head -n 1)"
    want="$(project "$desired" <<<"$desired")"
    if [ -z "$id" ]; then
        change "ruleset '$name': create"
        jq -r '.rules[] | "          rule \(.type)"' <<<"$desired"
        if [ "$dry_run" = 0 ] && ! printf '%s' "$desired" | api -X POST "repos/$repo/rulesets" --input - >/dev/null; then
            [ -n "$optional" ] || { echo "apply-repo-settings: creating ruleset '$name' failed" >&2; exit 1; }
            say "warn    ruleset '$name' was rejected; see docs/REPO_SETTINGS.md for the fallback"
        fi
        return
    fi
    have="$(api "repos/$repo/rulesets/$id" | project "$desired")"
    if [ "$have" = "$want" ]; then
        same "ruleset '$name' (id $id)"
        return
    fi
    change "ruleset '$name' (id $id): update"
    diff <(printf '%s\n' "$have") <(printf '%s\n' "$want") | sed 's/^/          /' || true
    if [ "$dry_run" = 0 ] && ! printf '%s' "$desired" | api -X PUT "repos/$repo/rulesets/$id" --input - >/dev/null; then
        [ -n "$optional" ] || { echo "apply-repo-settings: updating ruleset '$name' failed" >&2; exit 1; }
        say "warn    ruleset '$name' was rejected; see docs/REPO_SETTINGS.md for the fallback"
    fi
}

checks_json="$(printf '%s\n' "${checks[@]}" | jq -R --argjson app "$GITHUB_ACTIONS_APP_ID" \
    '{context: ., integration_id: $app}' | jq -s 'sort_by(.context)')"
main_branch='{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}}'

apply_ruleset "$(jq -n --argjson checks "$checks_json" --argjson cond "$main_branch" '{
    name: "main: integrity",
    target: "branch",
    enforcement: "active",
    conditions: $cond,
    bypass_actors: [],
    rules: [
        {type: "deletion"},
        {type: "non_fast_forward"},
        {type: "pull_request", parameters: {
            required_approving_review_count: 0,
            dismiss_stale_reviews_on_push: false,
            require_code_owner_review: false,
            require_last_push_approval: false,
            required_review_thread_resolution: true,
            allowed_merge_methods: ["merge", "squash", "rebase"]}},
        {type: "required_status_checks", parameters: {
            strict_required_status_checks_policy: true,
            do_not_enforce_on_create: false,
            required_status_checks: $checks}}
    ]}')"

# Code-owner review for the security/networking paths in .github/CODEOWNERS.
# Repository admins may bypass THIS ruleset only through a pull request (a
# sole maintainer cannot approve their own pull request); required checks in
# "main: integrity" still apply to them.
apply_ruleset "$(jq -n --argjson cond "$main_branch" '{
    name: "main: security review",
    target: "branch",
    enforcement: "active",
    conditions: $cond,
    bypass_actors: [{actor_id: 5, actor_type: "RepositoryRole", bypass_mode: "pull_request"}],
    rules: [
        {type: "pull_request", parameters: {
            required_approving_review_count: 0,
            dismiss_stale_reviews_on_push: true,
            require_code_owner_review: true,
            require_last_push_approval: false,
            required_review_thread_resolution: true,
            allowed_merge_methods: ["merge", "squash", "rebase"]}}
    ]}')"

if [ "$skip_copilot" = 0 ]; then
    apply_ruleset "$(jq -n --argjson cond "$main_branch" '{
        name: "main: Copilot code review",
        target: "branch",
        enforcement: "active",
        conditions: $cond,
        bypass_actors: [],
        rules: [{type: "copilot_code_review", parameters: {
            review_on_push: true,
            review_draft_pull_requests: false}}]}')" optional
else
    say "skip    ruleset 'main: Copilot code review' (--skip-copilot)"
fi

apply_ruleset "$(jq -n '{
    name: "release tags",
    target: "tag",
    enforcement: "active",
    conditions: {ref_name: {include: ["refs/tags/v*"], exclude: []}},
    bypass_actors: [],
    rules: [{type: "deletion"}, {type: "non_fast_forward"}, {type: "update"}]}')"

say
say "Default branch: $default_branch. Required status checks:"
printf '          %s\n' "${checks[@]}"
if [ "$dry_run" = 1 ]; then
    say "apply-repo-settings: $changes change(s) would be made (dry run, nothing written)"
else
    say "apply-repo-settings: $changes change(s) applied"
fi
