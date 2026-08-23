#!/bin/sh
set -eu

die() { printf '%s\n' "$*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
exact_container_names() {
    names=$(docker ps -a --format '{{.Names}}') || die "cannot list Docker containers"
    printf '%s\n' "$names" | awk -v exact="$container" '$0 == exact'
}

[ "$#" -eq 4 ] || die "usage: $0 --approve-destructive-container INSTALLER /absolute/new/evidence CONTAINER"
[ "$1" = --approve-destructive-container ] || die "explicit destructive container approval is required"
installer=$2
evidence=$3
container=$4
case $installer in /*) ;; *) die "installer path must be absolute" ;; esac
case $evidence in /*) ;; *) die "evidence path must be absolute" ;; esac
case $container in
    [A-Za-z0-9]* ) ;;
    * ) die "container name is unsafe" ;;
esac
case $container in *[!A-Za-z0-9_.-]*) die "container name is unsafe" ;; esac
[ -f "$installer" ] && [ ! -L "$installer" ] || die "installer must be a non-symlink regular file"
[ ! -e "$evidence" ] && [ ! -L "$evidence" ] || die "evidence path already exists"
[ -d "$(dirname "$evidence")" ] || die "evidence parent does not exist"
for tool in docker git shasum; do
    command -v "$tool" >/dev/null 2>&1 || die "required host tool is missing: $tool"
done

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
probe=$script_dir/inside-aarch64-container.sh
repo_root=$(git -C "$script_dir/../../.." rev-parse --show-toplevel) || die "runner is not in a Git worktree"
[ -z "$(git -C "$repo_root" status --porcelain=v1 --untracked-files=all)" ] || die "runner worktree must be clean"
expected_installer_sha=9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179
[ "$(sha256 "$installer")" = "$expected_installer_sha" ] || die "installer digest mismatch"
image=ubuntu@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517
docker image inspect "$image" >/dev/null 2>&1 || die "pinned container image is not local"
[ -z "$(exact_container_names)" ] || die "exact container name already exists: $container"

umask 077
mkdir -m 0700 "$evidence"
printf '%s\n' "$container" >"$evidence/container.name"
printf '%s\n' "$image" >"$evidence/container.image"
printf '%s\n' "$expected_installer_sha" >"$evidence/installer.expected.sha256"
sha256 "$probe" >"$evidence/probe.sha256"
git -C "$repo_root" rev-parse HEAD >"$evidence/product-git-revision"
uname -sm >"$evidence/host-uname"
docker info --format '{{.OSType}}/{{.Architecture}}' >"$evidence/docker-server-platform"

set -- docker run --rm --pull never --name "$container" \
    --platform linux/arm64 \
    --network none \
    --mount "type=bind,src=$installer,dst=/input/nix-installer-aarch64-linux,readonly" \
    --mount "type=bind,src=$probe,dst=/probe.sh,readonly" \
    --mount "type=bind,src=$evidence,dst=/evidence" \
    "$image" /bin/sh /probe.sh --approve-destructive-container aarch64-linux
printf '%s\n' "$@" >"$evidence/container.argv"
set +e
"$@" >"$evidence/container-run.output" 2>&1
container_status=$?
set -e
printf '%s\n' "$container_status" >"$evidence/container-run.status"

printf '%s\n' "$container" >"$evidence/container-post-cleanup.name"
exact_container_names >"$evidence/container-post-cleanup.names"
cleanup_count=$(wc -l <"$evidence/container-post-cleanup.names" | tr -d ' ')
printf '%s\n' "$cleanup_count" >"$evidence/container-post-cleanup.count"
date -u '+%Y-%m-%dT%H:%M:%SZ' >"$evidence/container-post-cleanup.checked-at"
printf '%s\n' 'Immediate host query after the exact docker run --rm command returned.' >"$evidence/container-post-cleanup.provenance"
shasum -a 256 \
    "$evidence/container.argv" \
    "$evidence/container-run.status" \
    "$evidence/container-post-cleanup.name" \
    "$evidence/container-post-cleanup.names" \
    "$evidence/container-post-cleanup.count" \
    "$evidence/container-post-cleanup.checked-at" \
    "$evidence/container-post-cleanup.provenance" \
    >"$evidence/container-post-cleanup.sha256"
find "$evidence" -type f ! -name evidence.sha256 -print | LC_ALL=C sort |
    while IFS= read -r file; do shasum -a 256 "$file"; done >"$evidence/evidence.sha256"
[ "$cleanup_count" -eq 0 ] || die "exact container remained after docker run --rm"
exit "$container_status"
