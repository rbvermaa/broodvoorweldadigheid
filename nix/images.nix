# OCI images (spec "Builds with Nix", ADR 0002). Each package is a docker-loadable tar:
# the stream script only runs on Linux, so the tar is written on the Linux builder.
{
  pkgs,
  server,
  cli,
  postgresql,
}:
let
  inherit (pkgs) lib;
  uid = "1000";
  # kanidmd and its web UI only: kanidm_unixd hard-codes a bash path, which would put a shell
  # in the image. kanidmd embeds its own store path for the UI, so that path is rewritten to
  # $out; the name is unchanged, so the rewrite keeps the binary's byte length.
  kanidm =
    let
      upstream = pkgs.kanidm_1_12;
    in
    pkgs.runCommand upstream.name { meta.mainProgram = "kanidmd"; } ''
      mkdir -p $out/bin
      cp -r ${upstream}/ui $out/ui
      cp ${upstream}/bin/kanidmd $out/bin/kanidmd
      chmod u+w $out/bin/kanidmd
      LC_ALL=C sed -i "s|${builtins.baseNameOf upstream}|$(basename $out)|g" $out/bin/kanidmd
      if grep -q ${builtins.baseNameOf upstream} $out/bin/kanidmd; then exit 1; fi
    '';

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
      env = [ "PATH=${server}/bin" ];
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
      ];
      dirs."/backup" = "0750";
      volumes = [ "/backup" ];
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
        images = lib.concatMapStringsSep " " (name: "${name}=${images."${name}-image"}") [
          "server"
          "postgres"
          "kanidm"
          "caddy"
          "backup"
        ];
      }
      ''
        fail() {
          echo "$1" >&2
          exit 1
        }

        for entry in $images; do
          name=''${entry%%=*}
          image=''${entry#*=}
          dir="$TMPDIR/$name"
          mkdir -p "$dir/tar" "$dir/root"
          tar -xf "$image" -C "$dir/tar"
          config="$dir/tar/$(jq -r '.[0].Config' "$dir/tar/manifest.json")"
          for layer in $(jq -r '.[0].Layers[]' "$dir/tar/manifest.json"); do
            tar -xf "$dir/tar/$layer" -C "$dir/root"
          done
          root="$dir/root"

          user=$(jq -r '.config.User // ""' "$config")
          case "$user" in
            "" | 0 | 0:* | root | root:*) fail "$name image runs as root (User=\"$user\")" ;;
          esac

          found=$(find "$root" \( ${
            lib.concatMapStringsSep " -o " (shell: "-name ${shell}") shells
          } \) \( -type f -o -type l \) | sed "s|^$root||")
          if [ "$name" = postgres ]; then
            # initdb needs /bin/sh for popen() (ADR 0002); dash is the only shell allowed.
            [ "$(readlink "$root/bin/sh")" = "${lib.getExe pkgs.dash}" ] \
              || fail "postgres image: /bin/sh is not dash"
            found=$(echo "$found" | grep -v -e '^/bin/sh$' -e '^${pkgs.dash}/' || true)
          elif [ -e "$root/bin/sh" ] || [ -L "$root/bin/sh" ]; then
            fail "$name image has /bin/sh"
          fi
          if [ -n "$found" ]; then
            echo "$name image contains a shell:"
            echo "$found"
            exit 1
          fi
          echo "$name image: User=$user, $(find "$root" -type f | wc -l) files, shell check passed"
        done

        config_of() {
          echo "$TMPDIR/$1/tar/$(jq -r '.[0].Config' "$TMPDIR/$1/tar/manifest.json")"
        }
        # Prints the in-image path of $2 found on the image's PATH, or fails.
        on_path() {
          local path located=""
          path=$(jq -r '.config.Env[] | select(startswith("PATH=")) | ltrimstr("PATH=")' "$(config_of "$1")")
          [ -n "$path" ] || fail "$1 image: PATH is not set"
          for bin in ''${path//:/ }; do
            if [ -z "$located" ] && [ -x "$TMPDIR/$1/root$bin/$2" ]; then located="$bin/$2"; fi
          done
          [ -n "$located" ] || fail "$1 image: $2 is not on PATH ($path)"
          echo "$located"
        }

        server=$(on_path server server)
        "$TMPDIR/server/root$server" healthcheck --help | grep -q -- --url \
          || fail "server image: server healthcheck --help does not list --url"
        echo "server image: server on PATH; server healthcheck --help runs"

        for tool in rustic pg_dump pg_restore; do
          on_path backup "$tool" > /dev/null
        done
        if jq -e '.config.Env[] | select(startswith("RUSTIC_CACHE_DIR="))' "$(config_of backup)" > /dev/null; then
          fail "backup image: RUSTIC_CACHE_DIR conflicts with the --no-cache that cli passes to rustic"
        fi
        entry=$(jq -r '.config.Entrypoint[0]' "$(config_of backup)")
        "$TMPDIR/backup/root$entry" backup --help | grep -q restore-test \
          || fail "backup image: cli backup --help does not list restore-test"
        backup_dir=$(for layer in $(jq -r '.[0].Layers[]' "$TMPDIR/backup/tar/manifest.json"); do
          tar --numeric-owner -tvf "$TMPDIR/backup/tar/$layer"
        done | awk '$NF ~ /^(\.\/)?backup\/?$/ { print $1, $2 }')
        [ "$backup_dir" = "drwxr-x--- ${uid}/${uid}" ] \
          || fail "backup image: /backup must be drwxr-x--- ${uid}/${uid}, got '$backup_dir'"
        echo "backup image: rustic, pg_dump, pg_restore on PATH; cli backup --help runs; /backup owned by ${uid}"
        touch $out
      '';
}
