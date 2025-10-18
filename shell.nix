{ pkgs ? import <nixpkgs> {} }:

let
  scummvmTools = pkgs.stdenv.mkDerivation {
    pname = "scummvm-tools";
    version = "2.7.0";

    src = pkgs.fetchFromGitHub {
      owner = "scummvm";
      repo = "scummvm-tools";
      rev = "v2.7.0";
      sha256 = "1fvycm0gj2w2j4r8p20hzkvznd3sapzvcm22pxjhlhr1cx5d693c";
    };

    nativeBuildInputs = with pkgs; [
      pkg-config
    ];

    buildInputs = with pkgs; [
      zlib
      libpng
      libjpeg
      freetype
      libtheora
      libvorbis
      SDL2
    ];

    configurePhase = ''
      ./configure --prefix=$out
    '';

    buildPhase = ''
      make -j$NIX_BUILD_CORES
    '';

    installPhase = ''
      make install
    '';
  };

  lua32 = pkgs.stdenv.mkDerivation {
    pname = "lua";
    version = "3.2";

    src = pkgs.fetchurl {
      url = "https://www.lua.org/ftp/lua-3.2.tar.gz";
      sha256 = "sha256-v4vqvUHmXL+MtBxojsoFiP/4Hh5fZ8tCvTcOHsxYXDM=";
    };

    nativeBuildInputs = with pkgs; [ ];
    dontConfigure = true;

    buildPhase = ''
      make all
    '';

    installPhase = ''
      mkdir -p $out/bin
      mkdir -p $out/share/lua32

      # provide distinct binaries so they can coexist with lua5.1
      cp bin/lua $out/bin/lua32
      cp bin/luac $out/bin/luac32

      cp -r include lib doc $out/share/lua32/
    '';
  };

  gstPackages = [
    pkgs.gst_all_1.gstreamer
    pkgs.gst_all_1.gst-plugins-base
    pkgs.gst_all_1.gst-plugins-good
    pkgs.gst_all_1.gst-plugins-bad
    pkgs.gst_all_1.gst-libav
  ];

in pkgs.mkShell {
  packages = with pkgs; (
    [
    scummvmTools      # extraction tooling for reference data
    lua32             # legacy runtime matching the retail game's Lua 3.x lineage
    lua5_1            # many scripts target classic Lua semantics
    python3           # quick one-off analysis helpers
    p7zip             # archive handling when spelunking assets
    ripgrep           # fast code/asset search
    jq                # lightweight JSON inspection for reports
    git
    rsync
    zig               # build the LD_PRELOAD shim
    qemu              # user-mode emulation for 32-bit binaries
    gdb               # inspect qemu-i386 core dumps
    xdotool           # locate X11 windows for targeted capture
    rustc
    cargo
    rustfmt
    rust-analyzer
    pkg-config
    llvmPackages.libclang
    llvmPackages.libclang.lib
    llvmPackages.clang-unwrapped
    glibc.dev
    ffmpeg-full
    alsa-lib
    vulkan-loader
    wayland
    libxkbcommon
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
    xorg.libxcb
    xorg.libXrender
    xorg.libXext
    xorg.libXfixes
    xorg.libXinerama
    xorg.libXxf86vm
    xorg.libXtst
    xorg.xwininfo
    ]
    ++ gstPackages
  );

  shellHook = ''
    export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
      pkgs."vulkan-loader"
      pkgs.wayland
      pkgs.libxkbcommon
      pkgs.xorg.libX11
      pkgs.xorg.libXcursor
      pkgs.xorg.libXi
      pkgs.xorg.libXrandr
      pkgs.xorg.libxcb
      pkgs.xorg.libXrender
      pkgs.xorg.libXext
      pkgs.xorg.libXfixes
      pkgs.xorg.libXinerama
      pkgs.xorg.libXxf86vm
      pkgs.xorg.libXtst
      (pkgs.lib.getLib pkgs.gst_all_1.gstreamer)
      (pkgs.lib.getLib pkgs.gst_all_1.gst-plugins-base)
      (pkgs.lib.getLib pkgs.gst_all_1.gst-plugins-good)
      (pkgs.lib.getLib pkgs.gst_all_1.gst-plugins-bad)
      (pkgs.lib.getLib pkgs.gst_all_1.gst-libav)
    ]}:$LD_LIBRARY_PATH"

    export GST_PLUGIN_SYSTEM_PATH_1_0="${pkgs.lib.concatStringsSep ":" (map (pkg: "${pkgs.lib.getLib pkg}/lib/gstreamer-1.0") gstPackages)}"
    export GST_PLUGIN_PATH="$GST_PLUGIN_SYSTEM_PATH_1_0"
    export GST_PLUGIN_SCANNER="${pkgs.gst_all_1.gstreamer}/libexec/gstreamer-1.0/gst-plugin-scanner"

    if command -v git >/dev/null 2>&1; then
      REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
    else
      REPO_ROOT="$(pwd)"
    fi
    export DEV_INSTALL_PATH="$REPO_ROOT/dev-install"

    if [ -z "$GRIM_INSTALL_PATH" ]; then
      export GRIM_INSTALL_PATH="$DEV_INSTALL_PATH"
    fi

    export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
    export BINDGEN_EXTRA_CLANG_ARGS="-I${pkgs.glibc.dev}/include"
  '';
}
