# linkctl

`linkctl` is a Linux command-line controller for the fixed-mount Insta360 Link 2C Pro. The project is capability-driven: it uses standard Linux camera and audio interfaces first, verified device profiles second, and explicitly labelled host-side processing where appropriate.

The current CLI provides read-only hardware discovery and reusable probe bundles:

```sh
linkctl device list
linkctl --device /dev/video2 device probe
linkctl --device /dev/video2 device probe --bundle golden-probe
```

Probes inventory USB identity, associated Linux nodes, V4L2 capabilities/formats/controls, UVC Extension Units through safe `GET_LEN`/`GET_INFO` queries, and ALSA/PipeWire audio capabilities. Serials are redacted unless `--include-serial` is explicit. See the [hardware probe guide](docs/hardware-probe.md) for bundle contents and limitations.

## Development

The development environment is managed by [devenv](https://devenv.sh/):

```sh
devenv shell
cargo test --workspace --all-features --locked
cargo run -p link-cli --bin linkctl -- --help
```

Before submitting a change, run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo deny --all-features check
```

## Safety boundary

Normal builds do not expose raw Extension Unit writes, driver detach, USB reset, firmware or calibration writes, or mechanical movement commands. Configuration cannot enable code that is absent from the build. A profile is never sufficient evidence for a write until it has been validated against the exact device, descriptor, and firmware under a separately reviewed implementation.

`device list` and `device probe` open device nodes only for read-only inventory. They never set a V4L2 format or control and never issue a UVC `SET_CUR` request.

Machine output uses schema version 1. JSON and JSON Lines errors always include `schema_version`, `ok`, `command`, `device`, `result`, and `error`.

See [CONTRIBUTING.md](CONTRIBUTING.md), the [threat model](docs/threat-model.md), and the [architecture decisions](docs/adr/) for the engineering contract.
