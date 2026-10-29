#!/bin/sh
# Builds rathole at the given tags, downloads frp and runs the tunnel
# benchmark against all of them. Run it from the repository root:
#
#   benches/scripts/compare.sh v0.6.1 v0.6.2 v0.6.3
#
# FRP_VERSION selects the frp release (default: latest). WORK_DIR keeps the
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
FRP_VERSION=${FRP_VERSION:-$(gh release view -R fatedier/frp --json tagName -q .tagName)}
FRP_VERSION=${FRP_VERSION#v}
FRP_NAME="frp_${FRP_VERSION}_linux_$FRP_ARCH"
FRP_DIR="$WORK_DIR/$FRP_NAME"
if [ ! -x "$FRP_DIR/frps" ]; then
	echo "Downloading frp $FRP_VERSION" >&2
	url="https://github.com/fatedier/frp/releases/download/v$FRP_VERSION"
	curl -fsSL -o "$WORK_DIR/$FRP_NAME.tar.gz" "$url/$FRP_NAME.tar.gz"
	curl -fsSL -o "$WORK_DIR/frp_sha256_checksums.txt" "$url/frp_sha256_checksums.txt"
	(cd "$WORK_DIR" && grep " $FRP_NAME.tar.gz\$" frp_sha256_checksums.txt | sha256sum -c -)
	tar -xzf "$WORK_DIR/$FRP_NAME.tar.gz" -C "$WORK_DIR"
fi
ARGS="$ARGS --frp v$FRP_VERSION=$FRP_DIR"

# shellcheck disable=SC2086
cargo bench --bench tunnel -- $ARGS "$@"
