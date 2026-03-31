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
    { nixpkgs, ... }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
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
            nativeBuildInputs = [ pyPkgs.setuptools pyPkgs.wheel ];
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
          tweetPython = pkgs.python312.withPackages (
            ps: [
              ps.tomlkit
              ps."tomli-w"
              twitterApiClient
            ]
          );
          archivr_unwrapped = pkgs.rustPlatform.buildRustPackage {
            pname = "archivr";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            cargoHash = "sha256-4m+4SMYA/rJ0eHEOc32zA2VdZI1pqzB5NenD0R0f2zM=";
            nativeBuildInputs = [ pkgs.pkg-config ];
          };
          archivr = pkgs.stdenv.mkDerivation {
            pname = "archivr-wrapped";
            version = "0.1.0";
            nativeBuildInputs = [ pkgs.makeWrapper ];
            buildInputs = [
              pkgs.yt-dlp
              tweetPython
            ];
            phases = [ "installPhase" ];
            installPhase = ''
              mkdir -p $out/bin $out/libexec/archivr
              cp -r ${archivr_unwrapped}/bin/* $out/bin/
              cp ${./vendor/twitter/scrape_user_tweet_contents.py} $out/libexec/archivr/scrape_user_tweet_contents.py
              chmod +x $out/libexec/archivr/scrape_user_tweet_contents.py
              for f in $out/bin/*; do
                mv "$f" "$f.orig"
                makeWrapper "$f.orig" "$f" \
                  --set ARCHIVR_YT_DLP ${pkgs.yt-dlp}/bin/yt-dlp \
                  --set ARCHIVR_TWEET_PYTHON ${tweetPython}/bin/python3 \
                  --set ARCHIVR_TWEET_SCRAPER $out/libexec/archivr/scrape_user_tweet_contents.py \
                  --prefix PATH : ${
                    lib.makeBinPath [
                      pkgs.yt-dlp
                      tweetPython
                    ]
                  }
              done
            '';
          };
        in
        {
          default = archivr;
          archivr = archivr;
          archivr-unwrapped = archivr_unwrapped;
        }
      );

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
            nativeBuildInputs = [ pyPkgs.setuptools pyPkgs.wheel ];
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
          tweetPython = pkgs.python312.withPackages (
            ps: [
              ps.tomlkit
              ps."tomli-w"
              twitterApiClient
            ]
          );
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
