# Browser tests in a NixOS VM without the NixOS test driver, which is Python (ADR 0033).
# The VM boots PostgreSQL, the server and geckodriver with headless Firefox; a one-shot
# unit runs the e2e binary, prints its exit code on the serial console and powers off.
# The check fails unless that exit code is 0.
{
  nixpkgs,
  pkgs,
  server,
  cli,
  e2e,
  axe,
  postgresql,
}:
let
  inherit (pkgs) lib;
  pgPort = 54329;
  databaseUrl = "postgres://postgres@127.0.0.1:${toString pgPort}/bvw";
  # Not a secret: the mock only checks the test_ prefix.
  mollieKey = pkgs.writeText "mollie-test-key" "test_e2eMockOnlyNotARealKey00000\n";

  machine =
    { config, ... }:
    {
      system.stateVersion = "26.11";
      networking.hostName = "e2e";
      virtualisation = {
        graphics = false;
        diskImage = null;
        memorySize = 3072;
        cores = 2;
        # No shared directories: with any, the runner starts virtiofsd through a Python
        # script. The store comes from an image instead, the result over the console.
        sharedDirectories = lib.mkForce { };
        useNixStoreImage = true;
        # The default qemu links brltty, spice and audio libraries that bring in Python.
        qemu.package = pkgs.qemu.override {
          hostCpuOnly = true;
          nixosTestRunner = true;
          brlttySupport = false;
        };
      };
      documentation.enable = false;
      nix.enable = false;
      system.disableInstallerTools = true;
      fonts.enableDefaultPackages = true;

      services.postgresql = {
        enable = true;
        package = postgresql;
        settings.port = pgPort;
        ensureDatabases = [ "bvw" ];
        authentication = lib.mkForce ''
          local all all trust
          host all all 127.0.0.1/32 trust
        '';
      };

      systemd.services.bvw-migrate = {
        wantedBy = [ "multi-user.target" ];
        requires = [ "postgresql-setup.service" ];
        after = [ "postgresql-setup.service" ];
        environment.DATABASE_URL = databaseUrl;
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = "${cli}/bin/cli migrate";
        };
      };

      systemd.services.bvw-server = {
        wantedBy = [ "multi-user.target" ];
        requires = [ "bvw-migrate.service" ];
        after = [ "bvw-migrate.service" ];
        environment = {
          DATABASE_URL = databaseUrl;
          BIND_ADDR = "127.0.0.1:8080";
          COOKIE_SECURE = "false";
          GALLERY = "true";
          # Staff sign in through the dev login; needs the loopback BIND_ADDR above.
          DEV_STAFF_LOGIN = "true";
          # https so the order confirmation is queued; the browser uses BIND_ADDR.
          PUBLIC_URL = "https://broodvoorweldadigheid.test";
          MOLLIE_API_KEY_FILE = "${mollieKey}";
          MOLLIE_API_URL = "http://127.0.0.1:8090/v2/";
        };
        serviceConfig.ExecStart = "${server}/bin/server";
      };

      systemd.services.geckodriver = {
        wantedBy = [ "multi-user.target" ];
        environment.MOZ_HEADLESS = "1";
        serviceConfig = {
          DynamicUser = true;
          PrivateTmp = true;
          Environment = "HOME=/tmp";
          # Unwrapped: the wrapper adds libcanberra and pipewire, which bring in Python via gstreamer.
          ExecStart = "${pkgs.geckodriver}/bin/geckodriver --host 127.0.0.1 --port 4444 --binary ${pkgs.firefox-unwrapped}/bin/firefox";
        };
      };

      systemd.services.e2e = {
        wantedBy = [ "multi-user.target" ];
        wants = [
          "bvw-server.service"
          "geckodriver.service"
        ];
        after = [
          "bvw-server.service"
          "geckodriver.service"
        ];
        path = [ config.systemd.package ];
        environment = {
          DATABASE_URL = databaseUrl;
          AXE_JS = "${axe}/axe.min.js";
        };
        serviceConfig = {
          Type = "oneshot";
          StandardOutput = "journal+console";
          StandardError = "journal+console";
        };
        script = ''
          set +e
          ${e2e}/bin/e2e
          code=$?
          if [ "$code" != 0 ]; then
            journalctl --no-pager -u bvw-migrate -u bvw-server -u geckodriver > /dev/console
          fi
          echo "E2E_EXIT_CODE=$code" > /dev/console
          systemctl poweroff --no-block
        '';
      };
    };

  closure = pkgs.closureInfo { rootPaths = [ vm.vm ]; };

  vm =
    (nixpkgs.lib.nixosSystem {
      modules = [
        "${nixpkgs}/nixos/modules/virtualisation/qemu-vm.nix"
        { nixpkgs.pkgs = pkgs; }
        machine
      ];
    }).config.system.build;
in
{
  inherit (vm) toplevel;

  check =
    pkgs.runCommand "e2e-vm"
      {
        requiredSystemFeatures = [ "kvm" ];
      }
      ''
        if grep -E "^/nix/store/[a-z0-9]{32}-(python|pypy)" ${closure}/store-paths; then
          echo "Python is in the closure of the VM or its runner (CLAUDE.md: no Python anywhere)"
          exit 1
        fi
        mkdir -p "$out"
        timeout 1200 ${vm.vm}/bin/run-e2e-vm </dev/null | tee "$out/console.log" || true
        code="$(grep -aoE "E2E_EXIT_CODE=[0-9]+" "$out/console.log" | tail -n 1 | cut -d= -f2)"
        if [ "$code" != 0 ]; then
          echo "e2e failed (exit code: ''${code:-none, the VM did not finish})"
          exit 1
        fi
      '';
}
