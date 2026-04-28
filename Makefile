APP_NAME := xbox_controller_macos_gip
INSTALL_NAME := powera-dolphin-pipe
PREFIX ?= /usr/local
BIN_DIR := $(PREFIX)/bin

.PHONY: build run install uninstall clean

build:
	CARGO_TARGET_DIR=target cargo build --release

run: build
	sudo ./target/release/$(APP_NAME)

install: build
	sudo install -m 0755 ./target/release/$(APP_NAME) "$(BIN_DIR)/$(INSTALL_NAME)"

uninstall:
	sudo rm -f "$(BIN_DIR)/$(INSTALL_NAME)"

clean:
	CARGO_TARGET_DIR=target cargo clean

