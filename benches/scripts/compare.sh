#!/bin/sh
# Builds rathole at the given tags, downloads frp and runs the tunnel
# benchmark against all of them. Run it from the repository root:
#
#   benches/scripts/compare.sh v0.6.1 v0.6.2 v0.6.3
#
# FRP_VERSIONS lists the frp releases to compare, "latest" is the newest one
# (default: latest), e.g. FRP_VERSIONS="0.38.0 latest". WORK_DIR keeps the
# binaries between runs (default: target/bench-compare). Extra arguments for
# the benchmark go after "--", e.g. "-- --duration 10".
set -eu

ROOT=$(pwd)
WORK_DIR=${WORK_DIR:-$ROOT/target/bench-compare}
mkdir -p "$WORK_DIR/bin"

TAGS=""
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
	TAGS="$TAGS $1"
	shift
done
[ "${1:-}" = "--" ] && shift

ARGS=""
for tag in $TAGS; do
	bin="$WORK_DIR/bin/rathole-$tag"
	if [ ! -x "$bin" ]; then
		echo "Building rathole $tag" >&2
		src="$WORK_DIR/src-$tag"
		git worktree add --detach "$src" "$tag" >&2
		if ! CARGO_TARGET_DIR="$WORK_DIR/target-$tag" cargo build --release --locked \
			--manifest-path "$src/Cargo.toml" >&2; then
			# Older tags have an outdated lock file or a time crate that
			# current Rust rejects
			cargo update --manifest-path "$src/Cargo.toml" -p time >&2
			CARGO_TARGET_DIR="$WORK_DIR/target-$tag" cargo build --release \
				--manifest-path "$src/Cargo.toml" >&2
		fi
		cp "$WORK_DIR/target-$tag/release/rathole" "$bin"
		git worktree remove --force "$src"
		rm -r "$WORK_DIR/target-$tag"
	fi
	ARGS="$ARGS --rathole $tag=$bin"
done

case "$(uname -m)" in
x86_64) FRP_ARCH=amd64 ;;
aarch64) FRP_ARCH=arm64 ;;
*) FRP_ARCH=$(uname -m) ;;
esac
for version in ${FRP_VERSIONS:-latest}; do
	if [ "$version" = latest ]; then
		version=$(gh release view -R fatedier/frp --json tagName -q .tagName)
	fi
	version=${version#v}
	name="frp_${version}_linux_$FRP_ARCH"
	dir="$WORK_DIR/$name"
	if [ ! -x "$dir/frps" ]; then
		echo "Downloading frp $version" >&2
		url="https://github.com/fatedier/frp/releases/download/v$version"
		curl -fsSL -o "$WORK_DIR/$name.tar.gz" "$url/$name.tar.gz"
		# Older releases have no checksum file
		if curl -fsSL -o "$WORK_DIR/frp_sha256_checksums.txt" "$url/frp_sha256_checksums.txt"; then
			(cd "$WORK_DIR" && grep " $name.tar.gz\$" frp_sha256_checksums.txt | sha256sum -c -)
		else
			echo "frp $version publishes no checksums, relying on HTTPS only" >&2
		fi
		tar -xzf "$WORK_DIR/$name.tar.gz" -C "$WORK_DIR"
	fi
	ARGS="$ARGS --frp v$version=$dir"
done

# shellcheck disable=SC2086
cargo bench --bench tunnel -- $ARGS "$@"
