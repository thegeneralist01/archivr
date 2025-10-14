{
  description = "Archivr - An open-source archive manager";

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
          archivr_unwrapped = pkgs.rustPlatform.buildRustPackage {
            pname = "archivr";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            cargoHash = "sha256-y47+Fmp3BID86aPnLtrvzg40lOr9cHyg/38+onisK7w=";
            nativeBuildInputs = [ pkgs.pkg-config ];
          };
          archivr = pkgs.stdenv.mkDerivation {
            pname = "archivr-wrapped";
            version = "0.1.0";
            nativeBuildInputs = [ pkgs.makeWrapper ];
            buildInputs = [
              pkgs.yt-dlp
            ];
            phases = [ "installPhase" ];
            installPhase = ''
              mkdir -p $out/bin
              cp -r ${archivr_unwrapped}/bin/* $out/bin/
              for f in $out/bin/*; do
                mv "$f" "$f.orig"
                makeWrapper "$f.orig" "$f" \
                  --set ARCHIVR_YT_DLP ${pkgs.yt-dlp}/bin/yt-dlp \
                  --prefix PATH : ${
                    lib.makeBinPath [
                      pkgs.yt-dlp
                    ]
                  }
              done
            '';
          };
        in
        {
          archivr = archivr;
          archivr-unwrapped = archivr_unwrapped;
        }
      );

      devShells = lib.genAttrs systems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            buildInputs = [
              pkgs.yt-dlp
              pkgs.nushell
            ];
            shellHook = ''
              export SHELL=${pkgs.nushell}/bin/nu
              echo "nushell dev shell active – yt-dlp on PATH"
              nu
            '';
          };
        }
      );
    };
}
