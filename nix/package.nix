{
  lib,
  stdenvNoCC,
  rustPlatform,
  bun,
  nodejs,
  cargo-tauri,
  pkg-config,
  wrapGAppsHook3,
  openssl,
  webkitgtk_4_1,
  glib-networking,
  libsoup_3,
  gtk3,
  dbus,
  gnome-keyring,
  libayatana-appindicator,
  alsa-lib,
}:

let
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../package.json
      ../bun.lock
      ../tsconfig.json
      ../vite.config.ts
      ../playwright.config.ts
      ../tests/e2e
      ../src
      ../crates
      ../vendor
      ../src-tauri/Cargo.toml
      ../src-tauri/build.rs
      ../src-tauri/tauri.conf.json
      ../src-tauri/capabilities
      ../src-tauri/icons
      ../src-tauri/sounds
      ../src-tauri/src
    ];
  };

  # Bun cannot fetch inside the sandbox, so dependencies are a fixed-output
  # derivation pinned by bun.lock. Update the hash when bun.lock changes.
  nodeModules = stdenvNoCC.mkDerivation {
    pname = "pigeon-node-modules";
    inherit version src;
    nativeBuildInputs = [ bun ];
    dontConfigure = true;
    buildPhase = ''
      runHook preBuild
      export HOME=$TMPDIR
      bun install --frozen-lockfile --ignore-scripts --no-progress \
        --os=${if stdenvNoCC.hostPlatform.isLinux then "linux" else "darwin"} \
        --cpu=${if stdenvNoCC.hostPlatform.isAarch64 then "arm64" else "x64"}
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      mkdir -p $out
      cp -R node_modules $out/
      runHook postInstall
    '';
    dontFixup = true;
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputHash =
      {
        x86_64-linux = "sha256-XHYBL7EugJDnme6phAGfhJK+crJfk8fYExOCnUz0Tjk=";
        aarch64-linux = "sha256-ZSRWnSDQEg8XQH2zVPHdxroy9dSzB+Swuh4jJdqocQY=";
      }
      .${stdenvNoCC.hostPlatform.system};
  };

  # Package scripts use `#!/usr/bin/env node`; nodejs in the build inputs lets
  # patchShebangs rewrite them. Node is build-time only.
  withNodeModules = ''
    cp -R ${nodeModules}/node_modules node_modules
    chmod -R u+w node_modules
    patchShebangs node_modules
  '';

  common = {
    inherit version src;
    cargoLock.lockFile = ../Cargo.lock;
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ openssl ];
  };
in
rustPlatform.buildRustPackage (
  common
  // {
    pname = "pigeon";

    nativeBuildInputs = common.nativeBuildInputs ++ [
      bun
      nodejs
      cargo-tauri.hook
      wrapGAppsHook3
    ];
    buildInputs = common.buildInputs ++ [
      webkitgtk_4_1
      glib-networking
      libsoup_3
      gtk3
      dbus
      libayatana-appindicator
      alsa-lib
    ];

    postPatch = withNodeModules;
    buildAndTestSubdir = "src-tauri";
    # Core tests run in their own check; the shell crate has none yet.
    doCheck = false;

    # The tray backend loads libayatana-appindicator at run time.
    preFixup = ''
      gappsWrapperArgs+=(--set-default WEBKIT_DISABLE_DMABUF_RENDERER 1)
      gappsWrapperArgs+=(--prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [ libayatana-appindicator ]})
    '';

    passthru = {
      inherit nodeModules;

      coreTests = rustPlatform.buildRustPackage (
        common
        // {
          pname = "pigeon-core-tests";
          cargoBuildFlags = [
            "-p"
            "pigeon-core"
          ];
          cargoTestFlags = [
            "-p"
            "pigeon-core"
          ];
          # Also exercise the real Secret Service backend in a throwaway
          # session: round trip, locked state, and no session bus.
          nativeCheckInputs = [
            dbus
            gnome-keyring
          ];
          postCheck = ''
            export HOME=$TMPDIR/home XDG_RUNTIME_DIR=$TMPDIR/run
            mkdir -p $HOME $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
            ss_test() { cargo test --release -p pigeon-core --test secret_service "$1" -- --exact --ignored; }
            DBUS_SESSION_BUS_ADDRESS= ss_test missing_session_bus_is_unavailable
            export -f ss_test
            dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- bash -c '
              echo -n throwaway-test-password | gnome-keyring-daemon --unlock --components=secrets >/dev/null
              ss_test round_trip_then_locked_state_is_explicit
            '
          '';
          installPhase = "touch $out";
        }
      );

      frontendChecks = stdenvNoCC.mkDerivation {
        pname = "pigeon-frontend-checks";
        inherit version src;
        nativeBuildInputs = [
          bun
          nodejs
        ];
        postPatch = withNodeModules;
        buildPhase = ''
          export HOME=$TMPDIR
          bun run typecheck
          bun run build
        '';
        installPhase = "touch $out";
      };
    };

    meta = {
      description = "Modern, local-first IMAP/SMTP email client";
      homepage = "https://github.com/kylescudder/pigeon";
      license = lib.licenses.mit;
      mainProgram = "pigeon";
      platforms = [
        "x86_64-linux"
        "aarch64-linux"
      ];
    };
  }
)
