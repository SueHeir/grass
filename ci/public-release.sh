#!/usr/bin/env bash
set -euo pipefail

MODE=${1:---check}
ROOT=${ECOSYSTEM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
OWNER=${GITHUB_OWNER:-SueHeir}
repos=(grass soil field dev_field_efvm dev_soil_peri dev_soil_sph dirt dev_couple_dem_cfd dev_couple_sph_cfd)

if [[ "$MODE" != "--check" && "$MODE" != "--push" ]]; then
  echo "usage: $0 [--check|--push]" >&2
  exit 2
fi

for repo in "${repos[@]}"; do
  dir="$ROOT/$repo"
  git -C "$dir" rev-parse --git-dir >/dev/null 2>&1 || {
    echo "missing sibling checkout: $dir" >&2; exit 2;
  }
  test -z "$(git -C "$dir" status --porcelain)" || {
    echo "$repo has uncommitted changes" >&2; exit 2;
  }
  if rg -n '192\.168\.|ssh://git@' "$dir" \
      --glob 'Cargo.toml' --glob '*.md' --glob '*.sh' --glob '!ci/public-release.sh' || \
     rg -n 'path = "(\.\./)+(grass|soil|field|dirt|dev_[^/]+)/' \
       "$dir" --glob 'Cargo.toml'; then
    echo "$repo contains a non-public dependency or link" >&2
    exit 2
  fi
  cargo metadata --manifest-path "$dir/Cargo.toml" --format-version 1 --no-deps >/dev/null
  git -C "$dir" fetch -q "git@github.com:$OWNER/$repo.git" main
  git -C "$dir" merge-base --is-ancestor FETCH_HEAD HEAD || {
    echo "$repo GitHub main has commits not present locally; merge them first" >&2
    exit 2
  }
  echo "PASS $repo $(git -C "$dir" rev-parse --short HEAD)"
done

# A public URL is not sufficient: a committed lockfile can keep a clean clone on
# an older ecosystem revision indefinitely. Verify every locked public-main
# dependency against the commit GitHub currently advertises for that repository.
for repo in "${repos[@]}"; do
  lock="$ROOT/$repo/Cargo.lock"
  git -C "$ROOT/$repo" ls-files --error-unmatch Cargo.lock >/dev/null 2>&1 || continue
  while read -r dependency locked; do
    public=$(git ls-remote "https://github.com/$OWNER/$dependency.git" refs/heads/main | awk '{print $1}')
    [[ -n "$public" && "$locked" == "$public" ]] || {
      echo "$repo/Cargo.lock pins $dependency at ${locked:0:8}, not public main ${public:0:8}" >&2
      echo "run: (cd $ROOT/$repo && cargo update -p <a-package-from-$dependency>)" >&2
      exit 2
    }
  done < <(
    sed -nE "s|.*git\+https://github.com/$OWNER/([^.]*)\.git\?branch=main#([0-9a-f]{40}).*|\1 \2|p" "$lock" |
      sort -u
  )
done

if [[ "$MODE" == "--push" ]]; then
  for repo in "${repos[@]}"; do
    dir="$ROOT/$repo"
    git -C "$dir" push "git@github.com:$OWNER/$repo.git" HEAD:main
    git -C "$dir" push origin HEAD:main
  done
fi
