install:
	cargo build --release
	sudo install -Dm755 target/release/trixie /usr/local/bin/trixie
	sudo install -Dm644 /usr/share/wayland-sessions/trixie.desktop /usr/share/wayland-sessions/trixie.desktop || true
	mkdir -p ~/.config/trixie
	cp -n config.toml ~/.config/trixie/config.toml

uninstall:
	sudo rm -f /usr/local/bin/trixie
	sudo rm -f /usr/share/wayland-sessions/trixie.desktop
