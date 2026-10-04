# NixOS module for archivr-server.
# Consumed by flake.nix as:
#   nixosModules.archivr-server = import ./modules/nixos/archivr-server.nix { inherit self; };
{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.archivr-server;

  # Escape characters that would break TOML double-quoted strings.
  escapeTOML = s: builtins.replaceStrings [ "\\" "\"" ] [ "\\\\" "\\\"" ] s;

  # Derived bind string from separate address + port options.
  # IPv6 addresses contain ":" and must be wrapped in brackets per RFC 2732.
  bindStr =
    let hasColon = builtins.match ".*:.*" cfg.listenAddress != null;
    in if hasColon
       then "[${cfg.listenAddress}]:${toString cfg.port}"
       else "${cfg.listenAddress}:${toString cfg.port}";

  # Generate the TOML registry file from NixOS options.
  # The auth DB is pinned to the StateDirectory so it survives upgrades.
  configFile = pkgs.writeText "archivr-server.toml" ''
    bind = "${bindStr}"
    auth_db_path = "/var/lib/archivr-server/archivr-auth.sqlite"

    ${lib.concatMapStrings (a: ''
      [[archives]]
      id = "${escapeTOML a.id}"
      label = "${escapeTOML a.label}"
      archive_path = "${escapeTOML a.path}"

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

    listenAddress = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      example = "0.0.0.0";
      description = ''
        IP address to listen on. Defaults to loopback only (127.0.0.1).
        Set to {code}`0.0.0.0` (or a specific interface address) to expose
        the server on the network. Always put a reverse proxy with TLS
        in front before doing this.
      '';
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 8080;
      description = "TCP port to listen on.";
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
            description = ''Absolute path to the .archivr directory created by {command}`archivr init`.
              The parent directory (which also contains the {file}`store/` blob directory)
              is whitelisted for read-write access under systemd hardening.'';
          };
          storePath = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = ''
              Absolute path to the store directory for this archive. Only needed
              when the archive was initialised with a custom store path outside
              the default sibling {file}`store/` location. When null (the default),
              the parent of {option}`path` already covers the standard layout.
            '';
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
        Open the firewall TCP port specified by
        {option}`services.archivr-server.port`. Only meaningful when
        {option}`services.archivr-server.listenAddress` is set to a
        non-loopback address.
      '';
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = lib.literalExpression ''
        {
          ARCHIVR_TRANSCRIBE_ENGINES = "whisper";
          ARCHIVR_WHISPER_CLI = "''${pkgs.whisper-cpp}/bin/whisper-cli";
          ARCHIVR_WHISPER_MODEL = "/var/lib/archivr-server/models/ggml-large-v3-turbo.bin";
        }
      '';
      description = ''
        Extra environment variables (e.g. ARCHIVR_TRANSCRIBE_ENGINES,
        ARCHIVR_PHONON2_CLI, LLM provider settings). Merged over defaults that
        point HOME, XDG_CACHE_HOME and HF_HOME into the state directory, so
        Python transcription engines can cache downloaded weights.
      '';
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Optional systemd EnvironmentFile (KEY=value lines) for settings that
        should stay out of the Nix store, such as LLM API keys.
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

      environment = {
        HOME = lib.mkDefault "/var/lib/archivr-server";
        XDG_CACHE_HOME = lib.mkDefault "/var/lib/archivr-server/.cache";
        HF_HOME = lib.mkDefault "/var/lib/archivr-server/.cache/huggingface";
      } // cfg.environment;

      serviceConfig = {
        ExecStart = "${cfg.package}/bin/archivr-server ${configFile}";
        User = cfg.user;
        Group = cfg.group;
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;

        # State directory — auth SQLite lives here across upgrades/restarts.
        StateDirectory = "archivr-server";
        StateDirectoryMode = "0750";

        Restart = "on-failure";
        RestartSec = "5s";

        # Hardening — make the entire FS read-only except for the state
        # directory and the parent directory of each mounted archive.
        # Each archive_path is an .archivr dir; its sibling store/ dir (where
        # capture artifacts are written) lives at the same level. Whitelisting
        # the parent covers both without over-permissioning.
        # GPU transcription engines (CUDA) need /dev/nvidia*; adding
        # PrivateDevices or DeviceAllow here would break them.
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ReadWritePaths =
          [ "/var/lib/archivr-server" ]
          ++ (map (a: builtins.dirOf a.path) cfg.archives)
          ++ (lib.concatMap (a: lib.optional (a.storePath != null) a.storePath) cfg.archives);
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        LockPersonality = true;
        RestrictSUIDSGID = true;
        RemoveIPC = true;
      };
    };

    networking.firewall = lib.mkIf cfg.openFirewall {
      allowedTCPPorts = [ cfg.port ];
    };
  };
}
