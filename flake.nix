{
  description = "Archivr - An open-source archive manager";

  nixConfig = {
    extra-substituters = [
      "https://cache.thegeneralist01.com/"
      "https://cache.nixos.org/"
    ];
    extra-trusted-public-keys = [
      "cache.thegeneralist01.com:jkKcenR877r7fQuWq6cr0JKv2piqBWmYLAYsYsSJnT4="
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
          # I Still Don't Care About Cookies (MV3) — unpacked Chromium extension
          # for dismissing cookie consent banners during headless captures.
          # Fetched from GitHub releases; update version + hash together.
          isdcac = pkgs.stdenv.mkDerivation {
            pname = "istilldontcareaboutcookies";
            version = "1.1.9";
            src = pkgs.fetchurl {
              url = "https://github.com/OhMyGuus/I-Still-Dont-Care-About-Cookies/releases/download/v1.1.9/ISDCAC-chrome-source.zip";
              hash = "sha256-j3CrlHyy0nT0AiqXD13Tzs2OwCsGDgUYe++e48sYu8s=";
            };
            nativeBuildInputs = [ pkgs.unzip ];
            sourceRoot = ".";
            installPhase = ''
              test -f manifest.json || { echo "ERROR: manifest.json not at extension root; zip structure may have changed"; exit 1; }
              mkdir -p $out
              cp -r . $out/
            '';
          };
          # yt-dlp — pinned to a specific GitHub release rather than pulled through
          # nixpkgs. Rationale: YouTube frequently rotates player-signature/API
          # surfaces, and yt-dlp ships updates on a days-to-weeks cadence; even
          # nixos-unstable often lags by months. When the binary is stale,
          # captures fail with HTTP 403 on formats the old client can't
          # authenticate. Fetching the zipapp directly (a Python zipapp with a
          # `#!/usr/bin/env python3` shebang) lets us bump the version + hash in
          # one place without waiting on nixpkgs. Wrapped so `python3` and
          # `ffmpeg` — the two runtime deps for muxed downloads — are always on
          # PATH regardless of the caller's environment.
          #
          # Bumping: replace `version`, then run `nix hash file <url>` on the
          # new zipapp URL and paste the sri output into `hash`.
          ytDlp = pkgs.stdenv.mkDerivation {
            pname = "yt-dlp";
            version = "2026.08.19";
            src = pkgs.fetchurl {
              url = "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/yt-dlp";
              hash = "sha256-H6ZzPDfqb7Ucma2P54Xnt+XzJGybmAIwMp1Pty7Y1NY=";
            };
            dontUnpack = true;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            installPhase = ''
              mkdir -p $out/bin
              install -m 0755 $src $out/bin/yt-dlp
              wrapProgram $out/bin/yt-dlp \
                --prefix PATH : ${lib.makeBinPath [ pkgs.python312 pkgs.ffmpeg ]}
            '';
          };
          # Frontend: per-system hash for the node_modules FOD.
          # bun installs platform-specific native binaries (esbuild, rollup),
          # so the hash differs between systems.
          # To compute the hash for a new system, set its entry to
          # "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=" and run:
          #   nix build .#archivr-server 2>&1 | grep "got:"
          # then paste the reported hash here.
          frontendDepsHash =
            {
              "aarch64-darwin" = "sha256-QYmiCaORbrWPVaM9xXViCZChSxwObRCjlrM03zukjQ0=";
              "x86_64-linux" = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
              "aarch64-linux" = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
            }
            .${system} or "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

          # FOD: fetch npm deps via bun.  Network is allowed; output is hashed.
          frontendDeps = pkgs.stdenv.mkDerivation {
            pname = "archivr-frontend-deps";
            version = "0.1.0";
            src = ./frontend;
            nativeBuildInputs = [ pkgs.bun ];
            buildPhase = ''
              export HOME=$TMPDIR
              bun install --frozen-lockfile
            '';
            installPhase = ''
              cp -r node_modules $out
            '';
            outputHash = frontendDepsHash;
            outputHashAlgo = "sha256";
            outputHashMode = "recursive";
          };

          # Build the Vite bundle using the pre-fetched node_modules.
          # Source files in frontend/src/ are jj-tracked and flow through automatically;
          # only the deps hash (above) needs updating when bun.lock/package.json changes.
          frontendStatic = pkgs.stdenv.mkDerivation {
            pname = "archivr-frontend-static";
            version = "0.1.0";
            src = ./frontend;
            nativeBuildInputs = [ pkgs.nodejs ];
            buildPhase = ''
              export HOME=$TMPDIR
              export BABEL_CACHE_PATH=$TMPDIR/babel-cache
              cp -r ${frontendDeps} node_modules
              chmod -R u+w node_modules
              node node_modules/vite/bin/vite.js build --outDir dist
            '';
            installPhase = ''
              cp -r dist $out
            '';
            dontFixup = true;
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
              ytDlp
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
                --set ARCHIVR_YT_DLP ${ytDlp}/bin/yt-dlp \
                --set ARCHIVR_SINGLE_FILE ${pkgs.single-file-cli}/bin/single-file \
                ${lib.optionalString pkgs.stdenv.isLinux "--set ARCHIVR_CHROME ${pkgs.chromium}/bin/chromium"} \
                --set ARCHIVR_TWEET_PYTHON ${tweetPython}/bin/python3 \
                --set ARCHIVR_TWEET_SCRAPER $out/libexec/archivr/scrape_user_tweet_contents.py \
                --set ARCHIVR_UBLOCK_EXT ${ublockLite} \
                --set ARCHIVR_COOKIE_EXT ${isdcac} \
                --prefix PATH : ${
                  lib.makeBinPath ([
                    ytDlp
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
            buildInputs = [ ytDlp tweetPython pkgs.single-file-cli ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.chromium ];
            phases = [ "installPhase" ];
            installPhase = ''
              mkdir -p $out/bin $out/libexec/archivr-server $out/share/archivr-server/static
              cp ${archivr_server_unwrapped}/bin/archivr-server $out/libexec/archivr-server/archivr-server
              cp ${./vendor/twitter/scrape_user_tweet_contents.py} $out/libexec/archivr-server/scrape_user_tweet_contents.py
              chmod +x $out/libexec/archivr-server/scrape_user_tweet_contents.py
              cp -r ${frontendStatic}/* $out/share/archivr-server/static/
              makeWrapper $out/libexec/archivr-server/archivr-server $out/bin/archivr-server \
                --set ARCHIVR_STATIC_DIR $out/share/archivr-server/static \
                --set ARCHIVR_YT_DLP ${ytDlp}/bin/yt-dlp \
                --set ARCHIVR_SINGLE_FILE ${pkgs.single-file-cli}/bin/single-file \
                ${lib.optionalString pkgs.stdenv.isLinux "--set ARCHIVR_CHROME ${pkgs.chromium}/bin/chromium"} \
                --set ARCHIVR_TWEET_PYTHON ${tweetPython}/bin/python3 \
                --set ARCHIVR_TWEET_SCRAPER $out/libexec/archivr-server/scrape_user_tweet_contents.py \
                --set ARCHIVR_UBLOCK_EXT ${ublockLite} \
                --set ARCHIVR_COOKIE_EXT ${isdcac} \
                --prefix PATH : ${lib.makeBinPath ([ ytDlp pkgs.single-file-cli tweetPython ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.chromium ])}
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
