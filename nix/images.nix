# OCI images (spec "Builds with Nix", ADR 0002). Each package is a docker-loadable tar:
# the stream script only runs on Linux, so the tar is written on the Linux builder.
{
  pkgs,
  server,
  cli,
}:
let
  inherit (pkgs) lib;
  uid = "1000";
  postgresql = pkgs.postgresql_17;
  kanidm = pkgs.kanidm_1_12;

  nss = pkgs.dockerTools.fakeNss.override {
    extraPasswdLines = [ "app:x:${uid}:${uid}:app:/var/empty:/sbin/nologin" ];
    extraGroupLines = [ "app:x:${uid}:" ];
  };

  baseEnv = [
    "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
    "TZDIR=${pkgs.tzdata}/share/zoneinfo"
    "TZ=Europe/Amsterdam"
  ];

  # dirs: { "/path" = "mode"; } created and owned by the app user.
  mkImage =
    {
      name,
      entrypoint,
      cmd ? [ ],
      env ? [ ],
      dirs ? { },
      ports ? [ ],
      volumes ? [ ],
      extraCommands ? "",
    }:
    let
      stream = pkgs.dockerTools.streamLayeredImage {
        name = "bvw-${name}";
        tag = "dev";
        contents = [
          pkgs.cacert
          pkgs.tzdata
        ];
        # Real files, not store symlinks: `docker exec` refuses an /etc/group symlink
        # that points outside the container root.
        extraCommands = ''
          mkdir -m 1777 tmp
          mkdir -p etc var/empty
          cp ${nss}/etc/passwd ${nss}/etc/group ${nss}/etc/nsswitch.conf etc/
          ${extraCommands}
        '';
        fakeRootCommands = lib.concatStrings (
          lib.mapAttrsToList (path: mode: ''
            mkdir -p -m ${mode} .${path}
            chown ${uid}:${uid} .${path}
          '') dirs
        );
        config = {
          User = "${uid}:${uid}";
          Entrypoint = entrypoint;
          Cmd = cmd;
          Env = baseEnv ++ env;
          ExposedPorts = lib.genAttrs (map (port: "${toString port}/tcp") ports) (_: { });
          Volumes = lib.genAttrs volumes (_: { });
        };
      };
    in
    pkgs.runCommand "bvw-${name}-image.tar" { passthru = { inherit stream; }; } "${stream} > $out";

  images = {
    server-image = mkImage {
      name = "server";
      entrypoint = [ (lib.getExe server) ];
      ports = [ 8080 ];
    };

    postgres-image = mkImage {
      name = "postgres";
      entrypoint = [
        (lib.getExe cli)
        "entrypoint"
        "postgres"
      ];
      env = [
        "PATH=${postgresql}/bin"
        "PGDATA=/var/lib/postgresql/data"
        "POSTGRES_USER=postgres"
      ];
      dirs = {
        "/var/lib/postgresql/data" = "0700";
        "/run/postgresql" = "0755";
      };
      # initdb and pg_ctl start their helper programs through popen(), which runs /bin/sh.
      extraCommands = ''
        mkdir -p bin
        ln -s ${lib.getExe pkgs.dash} bin/sh
      '';
      ports = [ 5432 ];
      volumes = [ "/var/lib/postgresql/data" ];
    };

    kanidm-image = mkImage {
      name = "kanidm";
      entrypoint = [
        (lib.getExe cli)
        "entrypoint"
        "kanidm"
      ];
      env = [
        "PATH=${kanidm}/bin"
        "KANIDM_BINDADDRESS=0.0.0.0:8443"
        "KANIDM_DB_PATH=/data/kanidm.db"
        "KANIDM_TLS_CHAIN=/data/chain.pem"
        "KANIDM_TLS_KEY=/data/key.pem"
        "KANIDM_ADMIN_BIND_PATH=/data/kanidmd.sock"
      ];
      dirs."/data" = "0750";
      ports = [ 8443 ];
      volumes = [ "/data" ];
    };

    caddy-image = mkImage {
      name = "caddy";
      entrypoint = [ (lib.getExe pkgs.caddy) ];
      cmd = [
        "run"
        "--config"
        "/etc/caddy/Caddyfile"
        "--adapter"
        "caddyfile"
      ];
      env = [
        "XDG_DATA_HOME=/data"
        "XDG_CONFIG_HOME=/config"
      ];
      dirs = {
        "/data" = "0750";
        "/config" = "0750";
      };
      ports = [
        80
        443
      ];
      volumes = [ "/data" ];
    };

    backup-image = mkImage {
      name = "backup";
      entrypoint = [ (lib.getExe cli) ];
      env = [
        "PATH=${pkgs.rustic}/bin:${postgresql}/bin"
        "RUSTIC_NO_PROGRESS=true"
        "RUSTIC_CACHE_DIR=/var/cache/rustic"
      ];
      dirs."/var/cache/rustic" = "0750";
    };
  };

  shells = [
    "sh"
    "bash"
    "dash"
    "ash"
    "zsh"
    "ksh"
    "mksh"
    "csh"
    "tcsh"
    "fish"
    "busybox"
    "toybox"
  ];
in
{
  packages = images;

  checks.image-contents =
    pkgs.runCommand "image-contents"
      {
        nativeBuildInputs = [ pkgs.jq ];
        image = images.server-image;
      }
      ''
        mkdir image root
        tar -xf "$image" -C image
        config=image/$(jq -r '.[0].Config' image/manifest.json)
        for layer in $(jq -r '.[0].Layers[]' image/manifest.json); do
          tar -xf "image/$layer" -C root
        done

        user=$(jq -r '.config.User // ""' "$config")
        case "$user" in
          "" | 0 | 0:* | root | root:*)
            echo "server image runs as root (User=\"$user\")"
            exit 1
            ;;
        esac

        if [ -e root/bin/sh ] || [ -L root/bin/sh ]; then
          echo "server image has /bin/sh"
          exit 1
        fi

        found=$(find root \( ${
          lib.concatMapStringsSep " -o " (shell: "-name ${shell}") shells
        } \) \( -type f -o -type l \))
        if [ -n "$found" ]; then
          echo "server image contains a shell:"
          echo "$found"
          exit 1
        fi

        echo "server image: User=$user, no shell in $(find root -type f | wc -l) files"
        touch $out
      '';
}
