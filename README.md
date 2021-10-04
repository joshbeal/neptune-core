# Neptune Core

Reference implementation for the Neptune protocol.

## Setup for Development
### Ubuntu
 - curl -- `apt install curl`
 - rustup -- `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh` (installs rustup, cargo, rustc etc.)
 - gnuplot -- `apt install gnuplot`
 - build-essential (for `make`) -- `apt install build-essential`

## Dev-ops Cheatsheet

 - To test, use `cargo test [start_of_test_name]`. Or, for a complete and slower build, run `make test`.
 - To run, use `make run` or `cargo run`.
 - To lint, use `make lint` or `cargo clippy`.
 - To format, use `make format` or `cargo fmt`.
 - To build, use `make build` or `cargo build`.
 - To install, use `make install` or `cargo install`.
 - To run lint, compile, run tests use `make all`. Note that this does *not* run install.
 - To see available command-line flags use `cargo run -- --help`

During development you can use `cargo` instead of `make` for the above commands. using `make` makes the compiler treat all warnings as errors, which we want for higher code quality. To send arguments to the Neptune Core program in a development setting use `cargo run -- [<flag> [<value>] [<flag> [<value>]]...]`, e.g. `cargo run -- --peers 8.8.101.69:9798 --peers 8.8.2.123:9798 --mine --listen-addr 10.64.111.55`.

## Logging
All logging is output to standard out. For development purposes it can sometimes be nice to get a more succint logging output by piping stdout through `sed` with the below command. This will only print the namespace of the logging event and the log text.
```
sed 's/.*neptune_core:\+\(.*\)/\1/g'
```


## Notes

The `Makefile` recipes set the flag `RUSTFLAGS=-Dwarnings` and this sometimes makes the recompilation **much** slower than without this flag, as `cargo` for some reason rebuilds the entire crate when this flag is set and a minor change is made in a test. So it is much faster to run the tests using cargo and then use the `make test` command before e.g. committing to ensure that the test build does not produce any warnings.
