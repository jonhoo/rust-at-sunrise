#!/bin/sh
rustup update nightly
if ! test -e target/debug/sunrise; then
	cargo b --bin sunrise
fi
exec target/debug/sunrise "$(rustc +nightly --version)" "$(cargo +nightly --version)"
