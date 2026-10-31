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
    mkdir -p $HOME $XDG_RUNTIME_DIR $HOME/.config/pigeon
    chmod 700 $XDG_RUNTIME_DIR
    # Exercises loading the tray library and creating the icon with no tray
    # host running, which must leave the app working.
    echo '{"tray": true}' > $HOME/.config/pigeon/settings.json
    dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- sh -c '
      echo -n throwaway-test-password | gnome-keyring-daemon --unlock --components=secrets >/dev/null
      PIGEON_SELFTEST=1 xvfb-run -a ${pigeon}/bin/pigeon 2> stderr.txt
    ' > report.json
    cat stderr.txt >&2
    if grep -q 'could not add the tray icon' stderr.txt; then
      echo 'the tray icon could not be created' >&2
      exit 1
    fi
    cat report.json
    mkdir -p $out
    cp report.json $out/
  ''
