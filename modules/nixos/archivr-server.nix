# NixOS module for archivr-server.
# Consumed by flake.nix as:
#   nixosModules.archivr-server = import ./modules/nixos/archivr-server.nix { inherit self; };
{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.archivr-server;

  # Generate the TOML registry file from NixOS options.
  # The auth DB is pinned to the StateDirectory so it survives upgrades.
  configFile = pkgs.writeText "archivr-server.toml" ''
    bind = "${cfg.bind}"
    auth_db_path = "/var/lib/archivr-server/archivr-auth.sqlite"

    ${lib.concatMapStrings (a: ''
      [[archives]]
      id = "${a.id}"
      label = "${a.label}"
      archive_path = "${a.path}"

    '') cfg.archives}
  '';
in
{
  options.services.archivr-server = {
    enable = lib.mkEnableOption "archivr web server";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.system}.archivr-server;
      defaultText = lib.literalExpression "archivr-server from the archivr flake";
      description = "The archivr-server package to use.";
    };

    bind = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1:8080";
      example = "0.0.0.0:8080";
      description = ''
        Address and port to listen on. Defaults to loopback only.
        Binding to a non-loopback address exposes the server on the network;
        put a reverse proxy with TLS and authentication in front before doing so.
      '';
    };

    archives = lib.mkOption {
      type = lib.types.listOf (lib.types.submodule {
        options = {
          id = lib.mkOption {
            type = lib.types.str;
            example = "personal";
            description = "Unique archive identifier used in the config and API URLs.";
          };
          label = lib.mkOption {
            type = lib.types.str;
            example = "Personal";
            description = "Display name shown in the UI archive switcher.";
          };
          path = lib.mkOption {
            type = lib.types.str;
            example = "/srv/archivr/personal/.archivr";
            description = "Absolute path to the .archivr directory created by {command}`archivr init`.";
          };
        };
      });
      default = [];
      description = ''
        Archives to mount. Each entry maps to one {code}`[[archives]]` block in the
        generated config file. The list must contain at least one entry.
      '';
      example = lib.literalExpression ''
        [
          { id = "personal"; label = "Personal"; path = "/srv/archivr/personal/.archivr"; }
          { id = "work";     label = "Work";     path = "/srv/archivr/work/.archivr"; }
        ]
      '';
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "archivr";
      description = "User account under which archivr-server runs.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "archivr";
      description = "Group under which archivr-server runs.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Open the firewall TCP port derived from
        {option}`services.archivr-server.bind`. Only meaningful when binding
        to a non-loopback address.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.archives != [];
        message = "services.archivr-server.archives must contain at least one archive.";
      }
    ];

    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      description = "archivr-server service user";
      home = "/var/lib/archivr-server";
      createHome = false; # StateDirectory creates it
    };
    users.groups.${cfg.group} = { };

    systemd.services.archivr-server = {
      description = "archivr web server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        ExecStart = "${cfg.package}/bin/archivr-server ${configFile}";
        User = cfg.user;
        Group = cfg.group;

        # State directory — auth SQLite lives here across upgrades/restarts.
        StateDirectory = "archivr-server";
        StateDirectoryMode = "0750";

        Restart = "on-failure";
        RestartSec = "5s";

        # Hardening — make the entire FS read-only except for the state
        # directory and the mounted archive directories.
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ReadWritePaths = [ "/var/lib/archivr-server" ] ++ (map (a: a.path) cfg.archives);
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        LockPersonality = true;
        RestrictNamespaces = true;
        RestrictSUIDSGID = true;
        RemoveIPC = true;
      };
    };

    networking.firewall = lib.mkIf cfg.openFirewall {
      allowedTCPPorts = [
        (lib.toInt (lib.last (lib.splitString ":" cfg.bind)))
      ];
    };
  };
}
