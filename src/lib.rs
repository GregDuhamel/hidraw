//! Linux hidraw for daemons: find a device's hidraw nodes through sysfs, talk
//! to it with feature, input and output reports, wait on it with a timeout,
//! and tell a device that went away from a transient error.
//!
//! ```no_run
//! use std::time::Duration;
//! use hidraw::{Bus, Device, Filter};
//!
//! # fn main() -> std::io::Result<()> {
//! // The control interface of a Razer Mouse Dock Pro: the USB one, not the
//! // mouse paired over Bluetooth nor the virtual battery a daemon publishes
//! // for it, both of which carry the same IDs.
//! let filter = Filter::new()
//!     .bus(Bus::Usb)
//!     .vendor(0x1532)
//!     .product(0x00a4)
//!     .interface(0);
//! let Some(node) = hidraw::discover(&filter)?.into_iter().next() else {
//!     return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "dock not connected"));
//! };
//! let dock = Device::open(&node.path)?;
//!
//! // A feature-report exchange: the request goes out as SET_REPORT, the
//! // device fills its report buffer, GET_REPORT reads it back. Byte 0 of
//! // every buffer is the report ID, 0 for a device without.
//! let mut request = [0u8; 91];
//! request[1..].copy_from_slice(&build_the_request());
//! dock.set_feature(&request)?;
//! std::thread::sleep(Duration::from_millis(2));
//! let mut reply = [0u8; 91];
//! let len = dock.get_feature(&mut reply)?;
//! println!("{len} bytes: {:02x?}", &reply[1..len]);
//!
//! // Spontaneous input reports come through read(); here, bounded.
//! let mut input = [0u8; 64];
//! if let Some(len) = dock.read_timeout(&mut input, Duration::from_secs(1))? {
//!     println!("input: {:02x?}", &input[..len]);
//! }
//! # Ok(()) }
//! # fn build_the_request() -> [u8; 90] { [0; 90] }
//! ```
//!
//! A long-running daemon tells a device that is gone for good - the node will
//! never work again, exit or wait for it to come back - from a request the
//! device merely did not take with [`is_gone`]:
//!
//! ```no_run
//! # use hidraw::Device;
//! # fn poll_the_device(dock: &Device) -> std::io::Result<()> { Ok(()) }
//! # let dock = Device::open(std::path::Path::new("/dev/hidraw0"))?;
//! if let Err(err) = poll_the_device(&dock) {
//!     if hidraw::is_gone(&err) {
//!         return Err(err); // unplugged: reopen later
//!     }
//!     eprintln!("transient: {err}"); // asleep, out of range, busy: retry
//! }
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # What is here, and what is not
//!
//! The crate is the transport only. Every protocol a device speaks over it -
//! a request's bytes, how a reply is told from the previous one, how long to
//! wait for it - stays with the daemon that knows the device. The crate
//! exists because three daemons each carried a copy of the transport, and
//! each had learned a different one of the kernel's pitfalls:
//!
//! - the same vendor and product IDs show up on several buses at once, and
//!   a composite device has one node per USB interface ([`discover`],
//!   [`Node`], [`Filter`]);
//! - the report ioctls encode the buffer length in the opcode, and expect
//!   the report ID in byte 0 ([`Device::set_feature`],
//!   [`Device::get_feature`], [`Device::get_input`]);
//! - `poll(2)` is cut short by any signal and must be taken up again with the
//!   time left ([`Device::wait_readable`], [`poll`]);
//! - the kernel queues 64 input reports per open handle, and a reply is only
//!   found once what came before it is gone ([`Device::drain`]);
//! - an unplugged device is `ENODEV` to an ioctl and `EIO` to `read(2)`, and
//!   `EIO` from an ioctl means something else entirely ([`is_gone`],
//!   [`DeviceGone`]).
//!
//! Linux only: hidraw is a Linux driver, and the crate does not build
//! elsewhere.

mod device;
mod discover;
mod error;
mod ioctl;
mod poll;

pub use device::{Device, INPUT_QUEUE_LEN};
pub use discover::{Bus, DEV_ROOT, Filter, Node, SYSFS_ROOT, discover, discover_in};
pub use error::{DeviceGone, is_gone};
pub use poll::{PollFd, PollFlags, poll};
