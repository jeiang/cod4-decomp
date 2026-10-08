#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Builds the two reference SM3 translators the sm3 render cross-check compares against:
#   mojo_drv  - MojoShader (zlib, icculus/mojoshader), SPIR-V profile
#   vkd3d_drv - vkd3d-shader 2.1 (Wine vkd3d, LGPL-2.1-or-later), linked statically into our own driver
# Third-party sources are fetched into a gitignored build dir, never vendored. Our driver sources: tools/crosscheck/.
#
# usage: scripts/build-crosscheck-tools.sh [builddir]      (default: target/crosscheck-tools)
# then:  export COD4E_MOJOSHADER_DRV=<builddir>/bin/mojo_drv COD4E_VKD3D_DRV=<builddir>/bin/vkd3d_drv
#        COD4_PATH=/path/to/COD4 cargo test -p sm3 --test crosscheck -- --nocapture
# Needs: git, curl, tar, nix (for flex, bison, make, pkg-config, spirv-headers, vulkan-headers, and gcc when there is
# no `cc`), and a C compiler.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-$ROOT/target/crosscheck-tools}
MOJO_REV=ad5dff84830c2863c841f4b1f4e3df78c705b383
VKD3D_VER=2.1

if [ -z "${CROSSCHECK_IN_NIX:-}" ]; then
  need=""
  command -v cc >/dev/null 2>&1 || need="$need nixpkgs#gcc"
  # Always nixpkgs' flex/bison: vkd3d's grammars need bison >= 3 and macOS ships 2.3.
  need="$need nixpkgs#flex nixpkgs#bison nixpkgs#gnumake nixpkgs#pkg-config nixpkgs#perl"
  if [ -z "${SPIRV_HEADERS:-}" ]; then need="$need nixpkgs#spirv-headers"; fi
  if [ -z "${VULKAN_HEADERS:-}" ]; then need="$need nixpkgs#vulkan-headers"; fi
  if [ -n "$need" ]; then
    mkdir -p "$WORK"
    # shellcheck disable=SC2086
    exec env CROSSCHECK_IN_NIX=1 nix shell $need -c "$0" "$@"
  fi
fi
if [ -z "${SPIRV_HEADERS:-}" ]; then SPIRV_HEADERS=$(nix build nixpkgs#spirv-headers --no-link --print-out-paths | head -1); fi
if [ -z "${VULKAN_HEADERS:-}" ]; then VULKAN_HEADERS=$(nix build nixpkgs#vulkan-headers --no-link --print-out-paths | head -1); fi
# vkd3d's configure wants perl with the JSON module (system perl lacks it on some hosts).
PERL5LIB="$(nix build nixpkgs#perlPackages.JSON --no-link --print-out-paths | head -1)/lib/perl5/site_perl${PERL5LIB:+:$PERL5LIB}"
export PERL5LIB
CC=${CC:-cc}
mkdir -p "$WORK/bin" && cd "$WORK"

# --- MojoShader: SPIR-V profile only
if [ ! -d mojo/src ]; then
  mkdir -p mojo && git clone -q https://github.com/icculus/mojoshader mojo/src
  git -C mojo/src checkout -q $MOJO_REV
fi
printf '#define MOJOSHADER_VERSION 0\n#define MOJOSHADER_CHANGESET "%s"\n' "$(echo $MOJO_REV | cut -c1-7)" > mojo/src/mojoshader_version.h
FL="-DMOJOSHADER_EFFECT_SUPPORT -DSUPPORT_PROFILE_D3D=0 -DSUPPORT_PROFILE_BYTECODE=0 -DSUPPORT_PROFILE_HLSL=0 -DSUPPORT_PROFILE_GLSL120=0 -DSUPPORT_PROFILE_GLSLES=0 -DSUPPORT_PROFILE_GLSLES3=0 -DSUPPORT_PROFILE_GLSL=0 -DSUPPORT_PROFILE_ARB1=0 -DSUPPORT_PROFILE_ARB1_NV=0 -DSUPPORT_PROFILE_METAL=0"
(cd mojo/src && for f in mojoshader.c mojoshader_common.c mojoshader_effects.c profiles/mojoshader_profile_common.c profiles/mojoshader_profile_spirv.c; do
  $CC -O1 -w -c $FL -I. -Ispirv -Iprofiles $f -o ../$(basename $f .c).o
done)
$CC -O1 -w -I mojo/src -I mojo/src/spirv "$ROOT/tools/crosscheck/mojo_drv.c" mojo/*.o -lm -o bin/mojo_drv

# --- vkd3d-shader: only libvkd3d-shader; configure insists on libvulkan -> neutralise that check
V=vkd3d/vkd3d-$VKD3D_VER
if [ ! -f $V/.libs/libvkd3d-shader.a ]; then
  mkdir -p vkd3d
  [ -d $V ] || { (cd vkd3d && curl -fsSLO https://dl.winehq.org/vkd3d/source/vkd3d-$VKD3D_VER.tar.xz && tar xf vkd3d-$VKD3D_VER.tar.xz)
    L=$(grep -n "libvulkan and libMoltenVK not found" $V/configure | cut -d: -f1)
    sed -i.bak "${L}s/as_fn_error.*/: /" $V/configure; }
  (cd $V && ./configure --disable-tests --disable-doxygen-doc --disable-shared --enable-static CFLAGS="-O1 -w" \
      CPPFLAGS="-I$SPIRV_HEADERS/include -I$VULKAN_HEADERS/include" >/dev/null &&
    sed -i.bak 's/-flto=auto//g' Makefile && # LTO objects in a static archive need a plugin-aware ar
    make include/private/vkd3d_version.h include/private/spirv_grammar.h >/dev/null &&
    make -j"$(getconf _NPROCESSORS_ONLN)" libvkd3d-shader.la >/dev/null)
fi
$CC -O1 -w -I $V/include -I "$SPIRV_HEADERS/include" "$ROOT/tools/crosscheck/vkd3d_drv.c" \
  $V/.libs/libvkd3d-shader.a $V/.libs/libvkd3d-common.a -lm -lpthread -o bin/vkd3d_drv
echo "built: $WORK/bin/mojo_drv $WORK/bin/vkd3d_drv"
