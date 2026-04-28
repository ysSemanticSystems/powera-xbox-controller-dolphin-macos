APP_NAME := xbox_controller_macos_gip

.PHONY: build run clean

build:
	CARGO_TARGET_DIR=target cargo build --release

run: build
	sudo ./target/release/$(APP_NAME)

clean:
	CARGO_TARGET_DIR=target cargo clean

