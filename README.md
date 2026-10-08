# hidraw

[![CI](https://github.com/GregDuhamel/hidraw/actions/workflows/ci.yml/badge.svg)](https://github.com/GregDuhamel/hidraw/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Linux hidraw for daemons: find a device's hidraw nodes through sysfs, talk to
it with feature, input and output reports, wait on it with a timeout, and tell
a device that went away from a transient error.

It is the transport only. Every protocol a device speaks over it stays with
the daemon that knows the device. The crate exists because three daemons each
carried a copy of this transport, and each had learned a different one of the
kernel's pitfalls:

- the same vendor and product IDs show up on several buses at once (a dongle
  on USB, the same peripheral paired over Bluetooth, the virtual battery a
  daemon publishes for it through uhid), and a composite USB device has one
  node per interface;
- the report ioctls (`HIDIOCSFEATURE`, `HIDIOCGFEATURE`, `HIDIOCGINPUT`)
  encode the buffer length in the opcode, and want the report ID in byte 0;
- `poll(2)` is cut short by any signal and must be taken up again with the
  time left, and a sub-millisecond timeout must sleep, not spin;
- the kernel queues 64 input reports per open handle, and a reply is only
  found once what came before it is thrown away;
- an unplugged device is `ENODEV` to an ioctl and `EIO` to `read(2)`, while
  `EIO` from an ioctl is a transfer that merely failed.

One `unsafe` block, in the `ioctl` module, built on `rustix::ioctl`; no `libc`.

## Use

It is not on crates.io; depend on it through git, pinned to a release tag:

```toml
[dependencies]
hidraw = { git = "https://github.com/GregDuhamel/hidraw", tag = "v0.1.0" }
```

Releases are cut from the *Release* workflow (Actions → Release → Run workflow,
pick the semver bump): it runs the lints and tests, writes the version to
`Cargo.toml`, tags, and publishes the GitHub release.

```rust
use std::time::Duration;
use hidraw::{Bus, Device, Filter};

// The control interface of a Razer Mouse Dock Pro: the USB one, not the
// mouse paired over Bluetooth nor the virtual battery, which carry the
// same IDs.
let filter = Filter::new()
    .bus(Bus::Usb)
    .vendor(0x1532)
    .product(0x00a4)
    .interface(0);
let node = hidraw::discover(&filter)?
    .into_iter()
    .next()
    .ok_or("dock not connected")?;
let dock = Device::open(&node.path)?;

// A feature-report exchange: the request goes out as SET_REPORT, the device
// fills its report buffer, GET_REPORT reads it back. Byte 0 of every buffer
// is the report ID, 0 for a device without.
let mut request = [0u8; 91];
request[1..].copy_from_slice(&build_the_request());
dock.set_feature(&request)?;
std::thread::sleep(Duration::from_millis(2));
let mut reply = [0u8; 91];
let len = dock.get_feature(&mut reply)?;
println!("{len} bytes: {:02x?}", &reply[1..len]);

// Spontaneous input reports come through read(); here, bounded.
let mut input = [0u8; 64];
if let Some(len) = dock.read_timeout(&mut input, Duration::from_secs(1))? {
    println!("input: {:02x?}", &input[..len]);
}
```

A long-running daemon exits, or waits for the device to come back, when the
node will never work again, and retries otherwise:

```rust
if let Err(err) = poll_the_device(&dock) {
    if hidraw::is_gone(&err) {
        return Err(err.into()); // unplugged: reopen later
    }
    eprintln!("transient: {err}"); // asleep, out of range, busy: retry
}
```

### The API in one glance

| | |
|---|---|
| `discover(&Filter) -> Vec<Node>` | the nodes under `/sys/class/hidraw`, with bus, IDs, USB interface and name; `discover_in(sysfs, dev, ..)` takes the roots, for tests |
| `Filter::new().bus(..).vendor(..).product(..).interface(..)` | what to keep; an unset field matches anything |
| `Device::open(path)` | `O_RDWR \| O_CLOEXEC`, blocking; `Device::from_fd(fd, path)` wraps one systemd passed down |
| `set_feature(&[u8])`, `get_feature(&mut [u8])`, `get_input(&mut [u8])` | the report ioctls; `get_input` needs Linux 5.11 |
| `write(&[u8])` | one output report through `write(2)` |
| `read(&mut [u8])`, `read_timeout(.., Duration)`, `wait_readable(Duration)` | input reports; `EINTR` is retried with the time left |
| `drain()` | throws away the queued input reports and says how many |
| `is_gone(&io::Error)` | `ENODEV`, or the `EIO` that `read` marks as a `DeviceGone` |
| `poll(&mut [PollFd], Duration)` | `poll(2)` for a daemon that watches other descriptors next to the node |

## Who uses it

- [razerd](https://github.com/GregDuhamel/razerd) - the Razer Mouse Dock Pro's
  control interface: `set_feature` / `get_feature` for the request-reply
  protocol, `wait_readable` / `read` / `drain` to watch the mouse's input,
  `is_gone` to exit when the dock is unplugged.
- [thermaltaked](https://github.com/GregDuhamel/thermaltaked) - the LCD panel's
  two interfaces: `write` for the frame packets, `drain` then `read_timeout`
  for the acknowledgements.
- [HeadsetBatteryIndicator](https://github.com/GregDuhamel/HeadsetBatteryIndicator)
  - the Audeze Maxwell dongle: `discover` to tell the USB dongle from the
  virtual battery it publishes, `write` for the requests, `get_input` for the
  answers the dongle never pushes.

Next to [uhid-battery](https://github.com/GregDuhamel/uhid-battery), which
publishes the level a daemon read through this crate; neither depends on the
other.

## Development

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo test -- --ignored --nocapture   # lists and opens the machine's nodes; sends nothing
```

The unit tests need no hardware: discovery runs on a fake sysfs in a temporary
directory, the device I/O on a socket pair. The one `#[ignore]` test reads the
machine's real nodes and never writes to them.

## License

MIT, see [LICENSE](LICENSE).
