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

in pkgs.mkShell {
  packages = with pkgs; [
    scummvmTools      # extraction tooling for reference data
    lua5_1            # many scripts target classic Lua semantics
    python3           # quick one-off analysis helpers
    p7zip             # archive handling when spelunking assets
    ripgrep           # fast code/asset search
    jq                # lightweight JSON inspection for reports
    git
    rsync
    rustc
    cargo
    rustfmt
    rust-analyzer
    pkg-config
    alsaLib
    vulkan-loader
  ];

  shellHook = ''
    if [ -z "$GRIM_INSTALL_PATH" ]; then
      export GRIM_INSTALL_PATH="$HOME/.local/share/Steam/steamapps/common/Grim Fandango Remastered"
    fi

    echo "GRIM_INSTALL_PATH=$GRIM_INSTALL_PATH"
  '';
}
