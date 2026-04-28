APP_NAME := gipbridge

.PHONY: build run clean

build:
	CARGO_TARGET_DIR=target cargo build --release

run: build
	sudo ./target/release/$(APP_NAME)

clean:
	CARGO_TARGET_DIR=target cargo clean

