{
  pkgs,
  servedModule,
}:

pkgs.testers.runNixOSTest {
  name = "served-multi-user";

  nodes.machine =
    { lib, ... }:
    {
      imports = [ servedModule ];

      documentation.enable = false;
      environment.defaultPackages = lib.mkForce [ ];
      programs.nano.enable = false;

      users.users.alice = {
        isNormalUser = true;
        createHome = true;
      };
      users.users.bob = {
        isNormalUser = true;
        createHome = true;
      };

      services.served = {
        enable = true;
        users = [
          "alice"
          "bob"
        ];
      };
    };

  testScript = ''
    start_all()
    machine.wait_for_unit("served@alice.service")
    machine.wait_for_unit("served@bob.service")
    machine.succeed("test -S /home/alice/.local/state/served/runtime/served.sock")
    machine.succeed("test -S /home/bob/.local/state/served/runtime/served.sock")
    machine.succeed("test $(stat -c %U /home/alice/.local/state/served/runtime/served.sock) = alice")
    machine.succeed("test $(stat -c %U /home/bob/.local/state/served/runtime/served.sock) = bob")
    machine.succeed("systemctl stop served@alice.service")
    machine.succeed("systemctl is-active served@bob.service")
  '';
}
