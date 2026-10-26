{
  runCommand,
  pigeon,
  dbus,
  gnome-keyring,
  xvfb-run,
}:

runCommand "pigeon-selftest"
  {
    nativeBuildInputs = [
      dbus
      gnome-keyring
      xvfb-run
    ];
  }
  ''
    export HOME=$TMPDIR/home XDG_RUNTIME_DIR=$TMPDIR/run
    mkdir -p $HOME $XDG_RUNTIME_DIR
    chmod 700 $XDG_RUNTIME_DIR
    dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- sh -c '
      echo -n throwaway-test-password | gnome-keyring-daemon --unlock --components=secrets >/dev/null
      PIGEON_SELFTEST=1 xvfb-run -a ${pigeon}/bin/pigeon
    ' > report.json
    cat report.json
    mkdir -p $out
    cp report.json $out/
  ''
