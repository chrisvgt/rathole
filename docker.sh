#!/bin/sh
# docker.sh — run rathole's development and CI commands inside the toolchain
# container built from docker/dev.Dockerfile.
#
# The dev image is built on first use (and rebuilt automatically whenever
# docker/dev.Dockerfile changes, because its checksum is part of the tag) and
# then reused. Every command runs as the host user, with the checkout
# bind-mounted at its own absolute path and CARGO_HOME on a persistent named
# volume, so build artifacts and the cargo cache survive between invocations.
#
#   docker.sh build [args...]
#   docker.sh test  [args...]
#   docker.sh lint
#   docker.sh fmt   [--check]
#   docker.sh bench [args...]
#   docker.sh release [--target T ...]
#   docker.sh image
#   docker.sh run -- CMD...
#   docker.sh shell
#   docker.sh clean
#   docker.sh rebuild
set -eu

# Docker is required for every command; fail fast with a clear message when it
# is missing so the rest of the script never runs without it.
if ! command -v docker >/dev/null 2>&1; then
	echo "docker.sh: Docker was not found in PATH." >&2
	echo "docker.sh: install Docker (https://docs.docker.com/engine/install/) and try again." >&2
	exit 1
fi

ROOT=$(pwd)
UID_NUM=$(id -u)
GID_NUM=$(id -g)

DEV_DOCKERFILE="$ROOT/docker/dev.Dockerfile"
CARGO_HOME_VOLUME=rathole-cargo-home
# Marks that CARGO_HOME_VOLUME has been chowned to the current user, so the
# one-shot chown below is not re-run on every invocation.
CARGO_HOME_SENTINEL="$ROOT/.docker-cargo-home-uid"

# The dev image tag defaults to rathole-dev:<checksum of docker/dev.Dockerfile>,
# so a change to the Dockerfile yields a new tag and a fresh build. Override
# with RATHOLE_DEV_IMAGE.
if [ -n "${RATHOLE_DEV_IMAGE:-}" ]; then
	IMAGE=$RATHOLE_DEV_IMAGE
else
	IMAGE="rathole-dev:$(cksum "$DEV_DOCKERFILE" | awk '{print $1}')"
fi

usage() {
	cat >&2 <<'EOF'
Usage: docker.sh COMMAND [ARGS...]

Commands:
  build [args...]         cargo build --locked [args...]
  test  [args...]         run the test suites (no args) or one cargo test
  lint                    run clippy, license, and feature checks
  fmt   [--check]         cargo fmt (with -- --check when requested)
  bench [args...]         cargo bench --bench tunnel [args...]
  release [--target ...]  build release zips in dist/ (all four targets by default)
  image                   build the runtime image rathole:local
  run -- CMD...           run CMD inside the dev container
  shell                   run an interactive sh in the dev container
  clean                   cargo clean and remove dist/
  rebuild                 force a rebuild of the dev image
EOF
}

# Build the dev image if it does not exist yet, or when force is requested.
ensure_image() {
	if [ "${1:-}" = force ] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
		echo "docker.sh: building dev image $IMAGE" >&2
		docker build -f "$DEV_DOCKERFILE" -t "$IMAGE" "$ROOT"
	fi
}

# Resolve the Rust toolchain the dev image ships (e.g. 1.99.1) and pin the
# container to it. Without this, rustup honors the checkout's rust-toolchain.toml
# ("stable") and downloads a fresh toolchain into the throwaway container on
# every invocation — one that also lacks the musl target the image installs.
TOOLCHAIN=
resolve_toolchain() {
	[ -n "$TOOLCHAIN" ] && return
	TOOLCHAIN=$(docker image inspect "$IMAGE" --format '{{range .Config.Env}}{{println .}}{{end}}' |
		awk -F= '$1=="RUST_VERSION" {print $2}')
	TOOLCHAIN=${TOOLCHAIN:-stable}
}

# Make the CARGO_HOME named volume writable by the host user, once per
# (volume, uid:gid) pair. A fresh named volume is root-owned; chown it from a
# throwaway root container and record the result in a sentinel file so this
# does not run on every invocation.
ensure_cargo_home() {
	wanted="$UID_NUM:$GID_NUM"
	if [ -f "$CARGO_HOME_SENTINEL" ] && [ "$(cat "$CARGO_HOME_SENTINEL")" = "$wanted" ]; then
		return
	fi
	docker run --rm -v "$CARGO_HOME_VOLUME:/cargo" "$IMAGE" chown -R "$wanted" /cargo
	printf '%s\n' "$wanted" >"$CARGO_HOME_SENTINEL"
}

# Run the given command inside the dev container with the docker flags in $1
# (e.g. "-i -t"), returning its exit code. "$@" is the command to run.
_run() {
	flags=$1
	shift

	# Docker leaves HOME=/ when the user is a bare UID; point it at the
	# writable CARGO_HOME volume so Zig/cargo-zigbuild caches (and any other
	# $HOME/.cache users) have a writable, persistent location.
	# shellcheck disable=SC2086
	docker run --rm \
		--user "$UID_NUM:$GID_NUM" \
		-e CARGO_HOME=/cargo \
		-e HOME=/cargo \
		-e RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
		-e CARGO_TERM_COLOR \
		-e CI \
		-e GITHUB_ACTIONS \
		-e GITHUB_REF \
		-e GH_TOKEN \
		-e GITHUB_TOKEN \
		-e FRP_VERSIONS \
		-e RUST_LOG \
		-e RUST_BACKTRACE \
		-v "$ROOT:$ROOT" \
		-w "$ROOT" \
		-v "$CARGO_HOME_VOLUME:/cargo" \
		$flags \
		"$IMAGE" \
		"$@"
}

# Run a command in the dev container: allocate a TTY only when stdout is a TTY,
# and attach stdin only when stdin is a TTY.
dev_run() {
	ensure_image
	resolve_toolchain
	ensure_cargo_home
	flags=""
	[ -t 1 ] && flags="$flags -t"
	[ -t 0 ] && flags="$flags -i"
	_run "$flags" "$@"
}

# Like dev_run but always attaches stdin, so piped input reaches the container
# (used by `shell`).
dev_run_stdin() {
	ensure_image
	resolve_toolchain
	ensure_cargo_home
	flags="-i"
	[ -t 1 ] && flags="$flags -t"
	_run "$flags" "$@"
}

# Package one already-built target. UPX is applied best-effort (a size
# optimization; failure is not fatal). The per-target THIRD-PARTY-LICENSES.txt,
# the zip, and the checksum are produced inside the container so the host needs
# only Docker. The target name is the only varying input, passed as $1.
release_package() {
	target=$1
	# The script is single-quoted on purpose: it is passed verbatim to the
	# container shell, which expands $target, $bin and the $(...) itself.
	# shellcheck disable=SC2016
	dev_run sh -c '
		set -eu
		target=$1
		if [ "$target" = x86_64-unknown-linux-gnu ]; then
			bin=target/release/rathole
		else
			bin="target/$target/release/rathole"
		fi
		# UPX shrinks the binary but is optional; a failure here is not fatal.
		upx -q --best --lzma "$bin" || echo "docker.sh release: upx skipped for $target" >&2
		stage=".release-stage-$target"
		rm -rf "$stage"
		mkdir -p "$stage" dist
		cp "$bin" "$stage/rathole"
		cp LICENSE NOTICE "$stage/"
		if [ "$target" = x86_64-unknown-linux-gnu ]; then
			cargo about generate about.hbs -o "$stage/THIRD-PARTY-LICENSES.txt"
		else
			cargo about generate --no-default-features --features embedded about.hbs -o "$stage/THIRD-PARTY-LICENSES.txt"
		fi
		( cd "$stage" && zip -q "../dist/rathole-$target.zip" rathole LICENSE NOTICE THIRD-PARTY-LICENSES.txt )
		hash=$(sha256sum "dist/rathole-$target.zip")
		printf "%s  %s\n" "${hash%% *}" "rathole-$target.zip" > "dist/rathole-$target.zip.sha256"
		rm -rf "$stage"
	' -- "$target"
}

# Report a failing release target and stop. The cargo/zip output has already
# been streamed above; naming the target makes the failure unambiguous.
release_fail() {
	echo "docker.sh release: target $1 failed during $2; see the output above" >&2
	exit 1
}

# Build one release target: run its release-mode test (gnu/musl only), build
# it, then package it. A failure aborts with the target named.
release_target() {
	target=$1
	echo "docker.sh release: building $target" >&2
	case "$target" in
		x86_64-unknown-linux-gnu)
			dev_run cargo test --release --locked || release_fail "$target" test
			dev_run cargo build --release --locked || release_fail "$target" build
			;;
		x86_64-unknown-linux-musl)
			dev_run cargo test --release --locked --target "$target" --no-default-features --features embedded || release_fail "$target" test
			dev_run cargo build --release --locked --target "$target" --no-default-features --features embedded || release_fail "$target" build
			;;
		aarch64-unknown-linux-musl | armv7-unknown-linux-musleabihf)
			dev_run cargo zigbuild --release --locked --target "$target" --no-default-features --features embedded || release_fail "$target" build
			;;
		*)
			echo "docker.sh release: unknown target: $target" >&2
			exit 2
			;;
	esac
	release_package "$target" || release_fail "$target" package
}

if [ "$#" -eq 0 ]; then
	usage
	exit 2
fi
cmd=$1
shift

case "$cmd" in
build)
	dev_run cargo build --locked "$@"
	;;
test)
	if [ "$#" -eq 0 ]; then
		dev_run cargo test --locked
		dev_run cargo test --locked --no-default-features --features server,client,native-tls,noise,websocket-native-tls,hot-reload
		dev_run cargo test --locked --release --target x86_64-unknown-linux-musl --no-default-features --features embedded
	else
		dev_run cargo test --locked "$@"
	fi
	;;
lint)
	dev_run cargo clippy --all-targets -- -D warnings
	dev_run cargo clippy --all-targets --no-default-features --features server,client,native-tls,noise,websocket-native-tls,hot-reload -- -D warnings
	dev_run cargo about generate about.hbs -o target/THIRD-PARTY-LICENSES.txt
	dev_run cargo hack check --feature-powerset --no-dev-deps --mutually-exclusive-features default,native-tls,websocket-native-tls,rustls,websocket-rustls
	;;
fmt)
	case "${1:-}" in
	--check) dev_run cargo fmt -- --check ;;
	"") dev_run cargo fmt ;;
	*) echo "docker.sh fmt: unexpected argument: $1" >&2 && exit 2 ;;
	esac
	;;
bench)
	dev_run cargo bench --bench tunnel "$@"
	;;
release)
	targets=""
	while [ "$#" -gt 0 ]; do
		case "$1" in
			--target)
				[ "$#" -ge 2 ] || { echo "docker.sh release: --target requires a value" >&2; exit 2; }
				targets="$targets $2"
				shift 2
				;;
			*)
				echo "docker.sh release: unexpected argument: $1" >&2
				exit 2
				;;
		esac
	done
	if [ -z "$targets" ]; then
		targets="x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf"
	fi
	for target in $targets; do
		release_target "$target"
	done
	;;
image)
	docker build -f "$ROOT/docker/Dockerfile" -t rathole:local "$ROOT"
	;;
run)
	if [ "${1:-}" = "--" ]; then
		shift
	fi
	if [ "$#" -eq 0 ]; then
		echo "docker.sh run: expected a command after --" >&2
		exit 2
	fi
	dev_run "$@"
	;;
shell)
	dev_run_stdin sh
	;;
clean)
	dev_run cargo clean
	rm -rf dist
	;;
rebuild)
	ensure_image force
	;;
help | -h | --help)
	usage
	;;
*)
	echo "docker.sh: unknown command: $cmd" >&2
	usage
	exit 2
	;;
esac
