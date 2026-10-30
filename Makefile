PREFIX ?= $(HOME)/.local

build:
	cargo build --release

install: build
	install -Dm755 target/release/plars $(PREFIX)/bin/plars
	install -Dm644 data/language-specs/typst.lang $(PREFIX)/share/plars/language-specs/typst.lang
	install -Dm644 data/applications/io.github.plars.Plars.desktop $(PREFIX)/share/applications/io.github.plars.Plars.desktop

uninstall:
	rm -f $(PREFIX)/bin/plars $(PREFIX)/share/applications/io.github.plars.Plars.desktop
	rm -rf $(PREFIX)/share/plars

.PHONY: build install uninstall
