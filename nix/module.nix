{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.served;
  servedExe = lib.getExe cfg.package;
  mkService = user: {
    name = "served@${user}";
    value = {
      description = "served service manager for ${user}";
      wantedBy = [ "multi-user.target" ];
      after = [ "local-fs.target" ];
      reloadIfChanged = true;
      restartTriggers = [ cfg.package ];
      serviceConfig = {
        Type = "simple";
        User = user;
        ExecCondition = "${pkgs.coreutils}/bin/test ${user} != root";
        SetLoginEnvironment = true;
        WorkingDirectory = "~";
        ExecStart = "${pkgs.runtimeShell} -lc ${lib.escapeShellArg "exec ${servedExe} daemon"}";
        ExecStop = "${servedExe} shutdown";
        ExecReload = "${servedExe} daemon --handoff";
        Restart = "always";
        RestartSec = "1s";
        RestartPreventExitStatus = "75";
        SuccessExitStatus = "75";
        KillMode = "process";
        TimeoutStopSec = "30s";
        NoNewPrivileges = true;
      };
    };
  };
in
{
  options.services.served = {
    enable = lib.mkEnableOption "served per-user service managers";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.served;
      defaultText = lib.literalExpression "pkgs.served";
      description = "The served package to run and expose on PATH.";
    };

    users = lib.mkOption {
      type = lib.types.listOf (lib.types.strMatching "[A-Za-z_][A-Za-z0-9_.-]*");
      default = [ ];
      example = [
        "alice"
        "bob"
      ];
      description = "Existing non-root users that receive independent served managers.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.users != [ ];
        message = "services.served.users must contain at least one user";
      }
      {
        assertion = !(builtins.elem "root" cfg.users);
        message = "services.served.users must not contain root";
      }
      {
        assertion = builtins.length cfg.users == builtins.length (lib.unique cfg.users);
        message = "services.served.users must not contain duplicates";
      }
    ];

    environment.systemPackages = [ cfg.package ];
    systemd.services = builtins.listToAttrs (map mkService cfg.users);
  };
}
