#!/usr/bin/env bash
# Builds the effect-tsgo 0.51.1 language server (Go tsc with the Effect hooks) on microsoft/TypeScript
# 3e9236361fa9 (typescript@7.1.0-beta), the Go pin of bump E (R222). It is the recipe of the effect-tsgo build
# that made the Effect expectations and gate files at that pin (saved state batch effectTsgoNpRecipe: tsc
# 976a514ee85a, BUILD.json 697553170a7e). scripts/effect/build-oracle.sh is the same tool at pin N 673a5f17d713.
#
# usage: scripts/effect/build-oracle-np.sh <out-dir> [effect-src]
#   <out-dir>    new work dir. Result: <out-dir>/bin/tsc with the lib*.d.ts files beside it, <out-dir>/BUILD.json,
#                logs in <out-dir>/logs, each applied patch as a tree diff in <out-dir>/patches-Np. The sources stay
#                in <out-dir>/src (effect-tsgo clone, TypeScript clone in src/typescript).
#   effect-src   effect-tsgo git repo that has EFFECT_REV 47cb1ed7 (effect-tsgo 0.51.1)
#                (default target/continuation-r97-goport/effect051/effect-tsgo-0511 in the main checkout:
#                the parent of the git common dir, or /home/theo/Code/sandbox/ts-rust from a clone).
# env: TS_PIN (default 3e9236361fa97a9e6c685f4833ac457ae2d8c50a), TS_REPO (microsoft/TypeScript clone with the
#      pin; default ~/.explore/repos/microsoft__TypeScript@<first 12 of TS_PIN>),
#      ADAPT (default 1; 0 = no step 0 and no patches-np copies: the plain 0.51.1 stack, for the method check
#      at its own TypeScript a1ef42b9),
#      TRIMPATH (default 1; 0 = go build without -trimpath, the build of record, see below).
# Run it under systemd-run --user --collect --wait -p MemoryMax=16G -p MemorySwapMax=0 (go build uses several
# GB), or on a testbox. About 5 minutes with a warm Go cache.
#
# Steps, as effect-tsgo's repoctl builds the "typescript" provider release (_tools/repoctl/src/{submodules,build}.ts):
#   0. patches-np/effect-tsgo-0511-Np.patch on effect-tsgo: the shim tool, its typescript provider overlay and
#      the Symbol accessor changes, taken from effect-tsgo #809, #832, #861 and 0.56.0 (the release that targets
#      3e9236361fa9). No Effect rule change (bump E w2/eff report, section 1). patches-np/reference/ holds the
#      upstream #832 and #861 diffs that it was cut from; the script does not apply them.
#   1. every _patches/typescript patch in name order; a patch with a copy in patches-np/ uses that copy (003:
#      Signature.TypeParameters(), as effect-tsgo #861) (git apply --check, git apply). A patch that does not
#      apply goes through git apply --3way (index = work tree before it). The script stops on a conflict.
#      Each applied patch is saved as a diff of the trees before and after it (<out-dir>/patches-Np). For a
#      3-way patch, its +/- lines must equal the original patch's +/- lines (only context and offsets move).
#      At 3e9236361fa9, 018 and 025 apply 3-way and the other 25 apply clean. The 3-way step needs the 3
#      pre-image blobs of 018 and 025; the script fetches each one that the TypeScript clone lacks.
#   2. the diagnostics generator (GOWORK=off, -locproject/-locsource as the "typescript" provider).
#   3. gen_shims with the "typescript" provider overlay.
#   4. go build -tags=noembed -ldflags="-s -w" ./typescript/tsc/cmd/tsc (CGO_ENABLED=0), lib*.d.ts beside tsc.
#
# Reproducible: GOTOOLCHAIN is go1.27.1 (the toolchain of the 0.51.1 release). With -trimpath (the default) every
# out dir and host gives KNOWN_SHA below; a different sha with the default revisions stops the script (exit 3).
# TRIMPATH=0 is the build of record: without -trimpath, go build writes the source paths into tsc (the out dir,
# GOROOT and the module cache), so it gives 976a514ee85a only with out dir
# <main checkout>/target/continuation-r97-goport/upstream/bumpE/w2/eff/build-Np2, GOROOT /home/theo/.local/go
# and GOMODCACHE /home/theo/go/pkg/mod. Its BUILD.json is then byte-equal to the record (697553170a7e): the
# "lane copy" label of 003 is the record's text. The two builds differ only in those path strings. Checked on a
# testbox (2026-10-23, upstream/bumpE/r222/followups71.md): TRIMPATH=0 there gave 976a514ee85a and the record
# BUILD.json; the default gave KNOWN_SHA in two out dirs with two GOROOTs.
set -euo pipefail

if [[ $# -lt 1 || $1 == help || $1 == -h || $1 == --help ]]; then
  sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
  exit 0
fi

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
# Work data lives in the main checkout's target dir: the parent of the git common dir from the main
# checkout or any worktree. An independent clone has no work data, so it uses the main checkout path
# (the remote hosts and testboxes mirror it at the same path).
MAIN=$(dirname "$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir)")
[[ -d $MAIN/target/continuation-r97-goport ]] || MAIN=/home/theo/Code/sandbox/ts-rust
OUT=$(realpath -m "$1")
EFFECT_SRC=$(realpath -m "${2:-$MAIN/target/continuation-r97-goport/effect051/effect-tsgo-0511}")
FIX=$ROOT/scripts/effect/patches-np
EFFECT_REV=47cb1ed7704aff44cacaa0f4d2ef24de990cba0e
PIN=${TS_PIN:-3e9236361fa97a9e6c685f4833ac457ae2d8c50a}
TS_REPO=${TS_REPO:-$HOME/.explore/repos/microsoft__TypeScript@${PIN:0:12}}
ADAPT=${ADAPT:-1}
TRIMPATH=${TRIMPATH:-1}
# tsc sha256 of the -trimpath build with the default revisions (any out dir, any host).
KNOWN_SHA=34d171e06da28e0669e6fae594d2846754868cac31b15734d66aa0324b3afc51
export GOTOOLCHAIN=go1.27.1
export GIT_OPTIONAL_LOCKS=0

if ! git -C "$EFFECT_SRC" rev-parse -q --verify "$EFFECT_REV^{commit}" > /dev/null 2>&1; then
  echo "build-oracle-np: no effect-tsgo repo with $EFFECT_REV at $EFFECT_SRC; pass one as the effect-src argument" >&2
  exit 2
fi
if ! git -C "$TS_REPO" rev-parse -q --verify "$PIN^{commit}" > /dev/null 2>&1; then
  echo "build-oracle-np: no TypeScript repo with $PIN at $TS_REPO; set TS_REPO" >&2
  exit 2
fi
[[ -e $OUT/src ]] && { echo "build-oracle-np: $OUT/src exists; use a new out dir" >&2; exit 2; }
mkdir -p "$OUT/bin" "$OUT/logs" "$OUT/patches-Np"
SRC=$OUT/src
log() { echo "build-Np: $*"; }

git clone -q --shared --no-checkout "$EFFECT_SRC" "$SRC"
git -C "$SRC" checkout -q --detach "$EFFECT_REV"
[[ $ADAPT == 1 ]] && git -C "$SRC" apply "$FIX/effect-tsgo-0511-Np.patch"
rmdir "$SRC/typescript"
git clone -q --shared --no-checkout "$TS_REPO" "$SRC/typescript"
git -C "$SRC/typescript" checkout -q --detach "$PIN"
TS=$SRC/typescript
git -C "$TS" config user.email build@local
git -C "$TS" config user.name build
# git apply --3way needs the pre-image blobs that the 3-way patches name in their index lines: at 3e9236361fa9,
# 018 (autoimport aliasresolver.go 52955e92, fix.go 46423f35) and 025 (tsoptions tsconfigparsing.go af1e72ac7).
# zbook's pin clone (blob:none, many commits) has them; a depth-1 pin clone (testboxes) does not. Fetch each
# missing one by id from github.com/microsoft/TypeScript into src/typescript only.
for blob in 52955e92c3d094245f3fb4c71137773c4e7f2c8b 46423f35f8e3e5ee5e4143b48c2e3201c4f51791 \
  af1e72ac71017ef2673b90e62d793e17367b7e98; do
  if ! git -C "$TS" cat-file -e "$blob" 2> /dev/null; then
    log "fetch blob $blob (3-way pre-image) from github.com/microsoft/TypeScript"
    git -C "$TS" fetch -q --no-tags --no-write-fetch-head --filter=blob:none https://github.com/microsoft/TypeScript.git "$blob"
  fi
done

pm() { grep -E '^[-+]' "$1" | grep -vE '^(\+\+\+|---) ' | sort; }
results=()
for p in "$SRC"/_patches/typescript/*.patch; do
  name=$(basename "$p")
  [[ $ADAPT == 1 && -f $FIX/$name ]] && p=$FIX/$name
  git -C "$TS" add -A
  before=$(git -C "$TS" write-tree)
  if git -C "$TS" apply --check "$p" 2> "$OUT/logs/check-$name.txt"; then
    git -C "$TS" apply "$p"
    how=clean
  else
    git -C "$TS" apply --3way "$p" > "$OUT/logs/3way-$name.txt" 2>&1 || { cat "$OUT/logs/3way-$name.txt"; echo "conflict in $name" >&2; exit 4; }
    if git -C "$TS" diff --name-only --diff-filter=U | grep -q .; then echo "conflict in $name" >&2; exit 4; fi
    how=3way
  fi
  git -C "$TS" add -A
  after=$(git -C "$TS" write-tree)
  git -C "$TS" diff --binary "$before" "$after" > "$OUT/patches-Np/$name"
  if [[ $how == 3way ]] && ! diff <(pm "$p") <(pm "$OUT/patches-Np/$name") > "$OUT/logs/pm-$name.diff"; then
    echo "$name: the +/- lines differ from the original (logs/pm-$name.diff)" >&2; exit 5
  fi
  [[ $p == "$FIX/$name" ]] && how="$how (lane copy)"
  log "$name $how"
  results+=("\"$name\": \"$how\"")
done

log diagnostics
(cd "$TS/tsc/internal/diagnostics" && GOWORK=off go run generate.go -diagnostics ./diagnostics_generated.go \
  -loc ./loc_generated.go -locdir ./loc -locproject ../../../tools/LocProject.json \
  -locsource ./diagnosticMessages.generated.json) > "$OUT/logs/diagnostics.log" 2>&1

log shims
(cd "$SRC/_tools/gen_shims" && GOWORK=off go run . --repository-root "$SRC" --source-root "$TS/tsc" \
  --module-prefix github.com/microsoft/TypeScript/tsc \
  --provider-shim-prefix github.com/microsoft/TypeScript/tsc/shim \
  --extra-shim-root "$SRC/_tools/gen_shims/providers/typescript") > "$OUT/logs/gen_shims.log" 2>&1

log "go build"
trim=()
[[ $TRIMPATH == 1 ]] && trim=(-trimpath)
(cd "$SRC" && CGO_ENABLED=0 go build "${trim[@]}" -tags=noembed -ldflags="-s -w" -o "$OUT/bin/tsc" ./typescript/tsc/cmd/tsc) > "$OUT/logs/go-build.log" 2>&1
cp "$TS"/tsc/internal/bundled/libs/lib*.d.ts "$OUT/bin/"

version=$("$OUT/bin/tsc" --version)
sha=$(sha256sum "$OUT/bin/tsc" | cut -d' ' -f1)
{
  echo "{"
  echo "  \"effectRev\": \"$(git -C "$SRC" rev-parse HEAD)\","
  [[ $ADAPT == 1 ]] && echo "  \"effectPatch\": \"effect-tsgo-0511-Np.patch $(sha256sum "$FIX/effect-tsgo-0511-Np.patch" | cut -c1-16)\","
  echo "  \"typescriptPin\": \"$PIN\","
  echo "  \"go\": \"$(go env GOVERSION)\","
  echo "  \"goBuild\": \"GOTOOLCHAIN=go1.27.1 CGO_ENABLED=0 go build ${trim[*]}${trim[*]:+ }-tags=noembed -ldflags=-s -w\","
  echo "  \"version\": \"$version\","
  echo "  \"tscSha256\": \"$sha\","
  echo "  \"libs\": $(ls "$OUT/bin"/lib*.d.ts | wc -l),"
  echo "  \"patches\": {"
  (IFS=$'\n'; echo "${results[*]}") | sed 's/^/    /; $!s/$/,/'
  echo "  }"
  echo "}"
} > "$OUT/BUILD.json"
log "$OUT/bin/tsc: $version ($sha)"
if [[ $TRIMPATH == 1 && $ADAPT == 1 && $PIN == 3e9236361fa97a9e6c685f4833ac457ae2d8c50a && $sha != "$KNOWN_SHA" ]]; then
  log "the default revisions gave $sha, not KNOWN_SHA $KNOWN_SHA: the build is not reproducible here"
  exit 3
fi
