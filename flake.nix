{
  description = "Archivr - An open-source archive manager";

  nixConfig = {
    extra-substituters = [
      "https://cache.thegeneralist01.com/"
      "https://cache.garnix.io/"
      "https://cache.nixos.org/"
    ];
    extra-trusted-public-keys = [
      "cache.thegeneralist01.com:jkKcenR877r7fQuWq6cr0JKv2piqBWmYLAYsYsSJnT4="
      "cache.garnix.io:CTFPyKSLcx5RMJKfLo5EEPUObbA78b0YQ2DTCJXqr9g="
    ];
  };

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, self, ... }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    in
    {
      packages = lib.genAttrs systems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          pyPkgs = pkgs.python312Packages;
          twitterApiClient = pyPkgs.buildPythonPackage rec {
            pname = "twitter-api-client";
            version = "0.10.22";
            format = "setuptools";
            src = pkgs.fetchPypi {
              pname = "twitter_api_client";
              inherit version;
              hash = "sha256-S5KzQRDIQroc2bJsPLaKR9xocHKniqd9Z055CsC5rbQ=";
            };
            nativeBuildInputs = [
              pyPkgs.setuptools
              pyPkgs.wheel
            ];
            propagatedBuildInputs = [
              pyPkgs.aiofiles
              pyPkgs."nest-asyncio"
              pyPkgs.httpx
              pyPkgs.tqdm
              pyPkgs.orjson
              pyPkgs.m3u8
              pyPkgs.websockets
              pyPkgs.uvloop
            ];
            pythonImportsCheck = [ "twitter" ];
            doCheck = false;
          };
          tweetPython = pkgs.python312.withPackages (ps: [
            twitterApiClient
          ]);
          # uBlock Origin Lite (MV3) — unpacked Chromium extension for headless ad-blocking.
          # Fetched from the uBOL-home GitHub releases; update version + hash together.
          ublockLite = pkgs.stdenv.mkDerivation {
            pname = "ublock-origin-lite";
            version = "2026.705.2152";
            src = pkgs.fetchurl {
              url = "https://github.com/uBlockOrigin/uBOL-home/releases/download/2026.705.2152/uBOLite_2026.705.2152.chromium.zip";
              hash = "sha256-4TbvDYbkOkDuVK17TeAbLDBcgf9O6f/vh2buGbLu4XQ=";
            };
            nativeBuildInputs = [ pkgs.unzip ];
            sourceRoot = ".";
            installPhase = ''
              mkdir -p $out
              cp -r . $out/
            '';
          };
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          nativeBuildInputs = [ pkgs.pkg-config ];
          archivr_cli_unwrapped = pkgs.rustPlatform.buildRustPackage {
            pname = "archivr-cli";
            inherit
              version
              src
              cargoLock
              nativeBuildInputs
              ;
            buildInputs = [ pkgs.openssl ];
            cargoBuildFlags = [
              "-p"
              "archivr-cli"
            ];
            cargoTestFlags = [
              "-p"
              "archivr-cli"
            ];
          };
          archivr_server_unwrapped = pkgs.rustPlatform.buildRustPackage {
            pname = "archivr-server";
            inherit
              version
              src
              cargoLock
              nativeBuildInputs
              ;
            buildInputs = [ pkgs.openssl ];
            cargoBuildFlags = [
              "-p"
              "archivr-server"
            ];
            cargoTestFlags = [
              "-p"
              "archivr-server"
            ];
          };
          archivr = pkgs.stdenv.mkDerivation {
            pname = "archivr-wrapped";
            version = "0.1.0";
            nativeBuildInputs = [ pkgs.makeWrapper ];
            buildInputs = [
              pkgs.yt-dlp
              pkgs.single-file-cli
              tweetPython
            ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.chromium ];
            phases = [ "installPhase" ];
            installPhase = ''
              mkdir -p $out/bin $out/libexec/archivr
              cp ${archivr_cli_unwrapped}/bin/archivr $out/libexec/archivr/archivr
              cp ${./vendor/twitter/scrape_user_tweet_contents.py} $out/libexec/archivr/scrape_user_tweet_contents.py
              chmod +x $out/libexec/archivr/scrape_user_tweet_contents.py
              makeWrapper $out/libexec/archivr/archivr $out/bin/archivr \
                --set ARCHIVR_YT_DLP ${pkgs.yt-dlp}/bin/yt-dlp \
                --set ARCHIVR_SINGLE_FILE ${pkgs.single-file-cli}/bin/single-file \
                ${lib.optionalString pkgs.stdenv.isLinux "--set ARCHIVR_CHROME ${pkgs.chromium}/bin/chromium"} \
                --set ARCHIVR_TWEET_PYTHON ${tweetPython}/bin/python3 \
                --set ARCHIVR_TWEET_SCRAPER $out/libexec/archivr/scrape_user_tweet_contents.py \
                --set ARCHIVR_UBLOCK_EXT ${ublockLite} \
                --prefix PATH : ${
                  lib.makeBinPath ([
                    pkgs.yt-dlp
                    pkgs.single-file-cli
                    tweetPython
                  ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.chromium ])
                }
            '';
          };
          archivr_server = pkgs.stdenv.mkDerivation {
            pname = "archivr-server-wrapped";
            inherit version;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            buildInputs = [ tweetPython pkgs.single-file-cli ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.chromium ];
            phases = [ "installPhase" ];
            installPhase = ''
              mkdir -p $out/bin $out/libexec/archivr-server $out/share/archivr-server/static
              cp ${archivr_server_unwrapped}/bin/archivr-server $out/libexec/archivr-server/archivr-server
              cp ${./vendor/twitter/scrape_user_tweet_contents.py} $out/libexec/archivr-server/scrape_user_tweet_contents.py
              chmod +x $out/libexec/archivr-server/scrape_user_tweet_contents.py
              cp -r ${./crates/archivr-server/static}/* $out/share/archivr-server/static/
              makeWrapper $out/libexec/archivr-server/archivr-server $out/bin/archivr-server \
                --set ARCHIVR_STATIC_DIR $out/share/archivr-server/static \
                --set ARCHIVR_SINGLE_FILE ${pkgs.single-file-cli}/bin/single-file \
                ${lib.optionalString pkgs.stdenv.isLinux "--set ARCHIVR_CHROME ${pkgs.chromium}/bin/chromium"} \
                --set ARCHIVR_TWEET_PYTHON ${tweetPython}/bin/python3 \
                --set ARCHIVR_TWEET_SCRAPER $out/libexec/archivr-server/scrape_user_tweet_contents.py \
                --set ARCHIVR_UBLOCK_EXT ${ublockLite}
            '';
          };
          archivr-all = pkgs.symlinkJoin {
            name = "archivr-all";
            paths = [
              archivr
              archivr_server
            ];
          };
        in
        {
          default = archivr-all;
          archivr-all = archivr-all;
          archivr = archivr;
          archivr-cli = archivr;
          archivr-cli-unwrapped = archivr_cli_unwrapped;
          archivr-unwrapped = archivr_cli_unwrapped;
          archivr-server = archivr_server;
          archivr-server-unwrapped = archivr_server_unwrapped;
        }
      );

      nixosModules = {
        archivr-server = import ./modules/nixos/archivr-server.nix { inherit self; };
        default = self.nixosModules.archivr-server;
      };

      devShells = lib.genAttrs systems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          pyPkgs = pkgs.python312Packages;
          twitterApiClient = pyPkgs.buildPythonPackage rec {
            pname = "twitter-api-client";
            version = "0.10.22";
            format = "setuptools";
            src = pkgs.fetchPypi {
              pname = "twitter_api_client";
              inherit version;
              hash = "sha256-S5KzQRDIQroc2bJsPLaKR9xocHKniqd9Z055CsC5rbQ=";
            };
            nativeBuildInputs = [
              pyPkgs.setuptools
              pyPkgs.wheel
            ];
            propagatedBuildInputs = [
              pyPkgs.aiofiles
              pyPkgs."nest-asyncio"
              pyPkgs.httpx
              pyPkgs.tqdm
              pyPkgs.orjson
              pyPkgs.m3u8
              pyPkgs.websockets
              pyPkgs.uvloop
            ];
            pythonImportsCheck = [ "twitter" ];
            doCheck = false;
          };
          tweetPython = pkgs.python312.withPackages (ps: [
            twitterApiClient
          ]);
        in
        {
          default = pkgs.mkShell {
            buildInputs = [
              pkgs.yt-dlp
              pkgs.nushell
              pkgs.uv
              tweetPython
            ];
            shellHook = ''
              export SHELL=${pkgs.nushell}/bin/nu
              echo "nushell dev shell active – yt-dlp, uv, and tweet scraper Python on PATH"
              nu
            '';
          };
        }
      );
    };
}
