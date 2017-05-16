# rust-at-sunrise
Tool for posting daily updates on the latest available Rust Nightly

Will poll the [nightly
manifest](https://static.rust-lang.org/dist/channel-rust-nightly.toml)
every 30m and look for new releases of `rust` and `cargo`.
When one is found, it will tweet as
[@rust_at_sunrise](https://twitter.com/rust_at_sunrise) with a tweet of
the form:

> 2017-05-15 nightly has been released.<br />
> Changes in Rust: https://github.com/rust-lang/rust/compare/386b0b9d3...75b056812<br />
> Changes in Cargo: https://github.com/rust-lang/cargo/compare/fa7584c...13d92c6

`sunrise` needs to know the last nightly at the time execution to
correctly generate the GitHub URLs. Run with:
```console
$ env CONSUMER_SECRET=... ACCESS_TOKEN_SECRET=... \
  cargo run (rustc +nightly --version) (cargo +nightly --version)
```
