{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.oxide;
in
{
  options.programs.oxide = {
    enable = lib.mkEnableOption "oxide";
    package = lib.mkPackageOption pkgs "oxide" { };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
