# Disposable Linux environment for running Rust tests from a non-Linux host.
# Not a substitute for NixOS runtime checks.
FROM rust:1-trixie
RUN apt-get update && apt-get install -y --no-install-recommends \
      libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
      libssl-dev libayatana-appindicator3-dev librsvg2-dev libasound2-dev pkg-config \
      dbus dbus-x11 gnome-keyring libsecret-tools xvfb xauth \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN rustup component add clippy rustfmt
