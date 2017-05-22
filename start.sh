#!/bin/sh
rustup update nightly
exec cargo run --bin sunrise -- "$(rustc +nightly --version)" "$(cargo +nightly --version)"
