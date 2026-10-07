#!/bin/sh
# THROWAWAY: rebuilds the three C/C++ translators into $WORK (default /tmp/sm3wgsl-work). Uses Nix for flex/bison/meson.
# Exact commands used for the recorded results (macOS arm64). Sources are fetched, never vendored.
set -e
WORK=${WORK:-/tmp/sm3wgsl-work}; HERE=$(cd "$(dirname "$0")" && pwd); mkdir -p $WORK/bin && cd $WORK
# --- MojoShader (zlib) @ ad5dff84830c2863c841f4b1f4e3df78c705b383 : SPIR-V profile only
[ -d mojo/src ] || { mkdir -p mojo && git clone https://github.com/icculus/mojoshader mojo/src && git -C mojo/src checkout ad5dff84830c2863c841f4b1f4e3df78c705b383; }
echo '#define MOJOSHADER_VERSION 0
#define MOJOSHADER_CHANGESET "ad5dff8"' > mojo/src/mojoshader_version.h
FL="-DMOJOSHADER_EFFECT_SUPPORT -DSUPPORT_PROFILE_D3D=0 -DSUPPORT_PROFILE_BYTECODE=0 -DSUPPORT_PROFILE_HLSL=0 -DSUPPORT_PROFILE_GLSL120=0 -DSUPPORT_PROFILE_GLSLES=0 -DSUPPORT_PROFILE_GLSLES3=0 -DSUPPORT_PROFILE_GLSL=0 -DSUPPORT_PROFILE_ARB1=0 -DSUPPORT_PROFILE_ARB1_NV=0 -DSUPPORT_PROFILE_METAL=0"
(cd mojo/src && for f in mojoshader.c mojoshader_common.c mojoshader_effects.c profiles/mojoshader_profile_common.c profiles/mojoshader_profile_spirv.c; do clang -O1 -c $FL -I. -Ispirv -Iprofiles $f -o ../$(basename $f .c).o; done)
clang -O1 -I mojo/src -I mojo/src/spirv $HERE/mojo_drv.c mojo/*.o -lm -o bin/mojo_drv
# --- vkd3d-shader 2.1 (LGPL-2.1-or-later): only libvkd3d-shader; configure insists on libvulkan -> neutralise that check
if [ ! -f vkd3d/vkd3d-2.1/.libs/libvkd3d-shader.a ]; then
  mkdir -p vkd3d && (cd vkd3d && curl -sLO https://dl.winehq.org/vkd3d/source/vkd3d-2.1.tar.xz && tar xf vkd3d-2.1.tar.xz)
  L=$(grep -n "libvulkan and libMoltenVK not found" vkd3d/vkd3d-2.1/configure | cut -d: -f1); sed -i '' "${L}s/as_fn_error.*/: /" vkd3d/vkd3d-2.1/configure
  SH=$(nix build nixpkgs#spirv-headers --no-link --print-out-paths); VH=$(nix build nixpkgs#vulkan-headers --no-link --print-out-paths)
  (cd vkd3d/vkd3d-2.1 && nix shell nixpkgs#flex nixpkgs#bison nixpkgs#gnumake nixpkgs#pkg-config -c sh -c "./configure --disable-tests --disable-doxygen-doc --disable-shared --enable-static CFLAGS=-O1 CPPFLAGS='-I$SH/include -I$VH/include' && make include/private/vkd3d_version.h include/private/spirv_grammar.h && make -j8 libvkd3d-shader.la")
fi
SH=$(nix build nixpkgs#spirv-headers --no-link --print-out-paths)
clang -O1 -I vkd3d/vkd3d-2.1/include -I $SH/include $HERE/vkd3d_drv.c vkd3d/vkd3d-2.1/.libs/libvkd3d-shader.a vkd3d/vkd3d-2.1/.libs/libvkd3d-common.a -lm -lpthread -o bin/vkd3d_drv
# --- dxbc-spirv (MIT) @ c7f069701227dcb800a01bd93e42f8001b7543e7 : tools/dxbc_compiler handles SM3
[ -d dxbcspv ] || { git clone https://github.com/doitsujin/dxbc-spirv dxbcspv && git -C dxbcspv checkout c7f069701227dcb800a01bd93e42f8001b7543e7 && git -C dxbcspv submodule update --init --depth 1; }
(cd dxbcspv && nix shell nixpkgs#meson nixpkgs#ninja -c sh -c 'meson setup build -Denable_tools=true --buildtype=release && ninja -C build')
