//! Finding hidraw nodes through sysfs.
//!
//! Every `/dev/hidrawN` has a directory `/sys/class/hidraw/hidrawN` whose
//! `device` symlink points at the HID device that backs it. Two things are
//! read from there:
//!
//! - the HID device's `uevent`, where the kernel writes `HID_ID=bus:vendor:
//!   product` and `HID_NAME=...` (`hid_uevent` in `drivers/hid/hid-core.c`).
//!   This is the authoritative identity, and it exists for every HID bus, USB
//!   or not;
//! - for a USB device, the `bInterfaceNumber` attribute of the USB interface
//!   that the HID device hangs under. A composite device (a mouse dock with a
//!   control, a keyboard and a mouse interface; an LCD panel with two) has one
//!   hidraw node per interface, and the interface number is what tells them
//!   apart.
//!
//! The roots are parameters of [`discover_in`] so that a fake tree in a
//! temporary directory can stand in for sysfs under test.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::device::Device;

/// Where the kernel lists the hidraw class devices.
pub const SYSFS_ROOT: &str = "/sys/class/hidraw";

/// Where the device nodes live.
pub const DEV_ROOT: &str = "/dev";

/// The bus a HID device sits on: the first field of `HID_ID`, with the values
/// of `BUS_*` in `linux/input.h`.
///
/// It matters because the same vendor and product IDs show up on several
/// buses at once: a dongle on USB, the same peripheral paired over Bluetooth,
/// and the virtual battery a daemon publishes for it through uhid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Bus {
    /// `BUS_USB` (`0x0003`).
    Usb,
    /// `BUS_BLUETOOTH` (`0x0005`).
    Bluetooth,
    /// `BUS_VIRTUAL` (`0x0006`): uhid devices, among others.
    Virtual,
    /// `BUS_I2C` (`0x0018`): the touchpads and keyboards of most laptops.
    I2c,
    /// Any other bus, with its raw number.
    Other(u16),
}

impl Bus {
    /// The bus for a raw `BUS_*` number.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        match raw {
            0x0003 => Self::Usb,
            0x0005 => Self::Bluetooth,
            0x0006 => Self::Virtual,
            0x0018 => Self::I2c,
            other => Self::Other(other),
        }
    }

    /// The raw `BUS_*` number.
    #[must_use]
    pub const fn raw(self) -> u16 {
        match self {
            Self::Usb => 0x0003,
            Self::Bluetooth => 0x0005,
            Self::Virtual => 0x0006,
            Self::I2c => 0x0018,
            Self::Other(other) => other,
        }
    }
}

/// A hidraw node and the identity of the HID device behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Node {
    /// The device node, `/dev/hidrawN`.
    pub path: PathBuf,
    /// The bus the device sits on.
    pub bus: Bus,
    /// The vendor ID, as in `HID_ID`.
    pub vendor: u16,
    /// The product ID, as in `HID_ID`.
    pub product: u16,
    /// The USB interface number (`bInterfaceNumber`), for a USB device whose
    /// sysfs tree has one; `None` on every other bus.
    pub interface: Option<u8>,
    /// The name the kernel gives the device (`HID_NAME`), usually the USB
    /// manufacturer and product strings; empty when the uevent has none.
    pub name: String,
}

impl Node {
    /// Opens the node; see [`Device::open`].
    ///
    /// # Errors
    ///
    /// Those of [`Device::open`].
    pub fn open(&self) -> io::Result<Device> {
        Device::open(&self.path)
    }
}

/// What [`discover`] keeps. A field left unset matches anything.
///
/// ```
/// use hidraw::{Bus, Filter};
///
/// // The control interface of a Razer Mouse Dock Pro, on USB only.
/// let dock = Filter::new()
///     .bus(Bus::Usb)
///     .vendor(0x1532)
///     .product(0x00a4)
///     .interface(0);
/// # let _ = dock;
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Filter {
    vendor: Option<u16>,
    product: Option<u16>,
    bus: Option<Bus>,
    interface: Option<u8>,
}

impl Filter {
    /// A filter that matches every node.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            vendor: None,
            product: None,
            bus: None,
            interface: None,
        }
    }

    /// Keeps the nodes of this vendor.
    #[must_use]
    pub const fn vendor(mut self, vendor: u16) -> Self {
        self.vendor = Some(vendor);
        self
    }

    /// Keeps the nodes of this product.
    #[must_use]
    pub const fn product(mut self, product: u16) -> Self {
        self.product = Some(product);
        self
    }

    /// Keeps the nodes on this bus.
    #[must_use]
    pub const fn bus(mut self, bus: Bus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Keeps the nodes on this USB interface. A node without an interface
    /// number (any bus but USB) never matches once this is set.
    #[must_use]
    pub const fn interface(mut self, interface: u8) -> Self {
        self.interface = Some(interface);
        self
    }

    /// Whether `node` passes the filter.
    #[must_use]
    pub fn matches(&self, node: &Node) -> bool {
        self.vendor.is_none_or(|vendor| vendor == node.vendor)
            && self.product.is_none_or(|product| product == node.product)
            && self.bus.is_none_or(|bus| bus == node.bus)
            && self
                .interface
                .is_none_or(|interface| node.interface == Some(interface))
    }
}

/// Lists the hidraw nodes that pass `filter`, in node-number order, without
/// opening any of them.
///
/// Reads `/sys/class/hidraw` and names the nodes under `/dev`; see
/// [`discover_in`] for the rules.
///
/// # Errors
///
/// When `/sys/class/hidraw` cannot be listed. An empty list is not an error:
/// it is what a device that is not plugged in looks like.
pub fn discover(filter: &Filter) -> io::Result<Vec<Node>> {
    discover_in(Path::new(SYSFS_ROOT), Path::new(DEV_ROOT), filter)
}

/// As [`discover`], with the hidraw class directory and the device directory
/// given explicitly, so that a fake tree can stand in for sysfs under test.
///
/// Every entry of `sysfs_root` is a hidraw node; its `device/uevent` is read
/// for `HID_ID` and `HID_NAME`, and when the bus is USB the ancestors of
/// `device` are searched for a `bInterfaceNumber` attribute. Entries whose
/// uevent cannot be read or has no well-formed `HID_ID` are skipped: that is
/// what a node whose device was unplugged between the listing and the read
/// looks like, and a daemon that polls sysfs must not fail over it. The
/// kernel's vendor and product fields are 32 bits wide; a device whose IDs do
/// not fit in 16 (no USB, Bluetooth or I2C device has such IDs, only a
/// contrived uhid one) is skipped too.
///
/// # Errors
///
/// When `sysfs_root` cannot be listed.
pub fn discover_in(sysfs_root: &Path, dev_root: &Path, filter: &Filter) -> io::Result<Vec<Node>> {
    let mut entries = fs::read_dir(sysfs_root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    // Numeric order: `hidraw10` after `hidraw2`, as the kernel numbers them.
    entries.sort_by_cached_key(|name| (node_number(name), name.clone()));

    let mut nodes = Vec::new();
    for name in entries {
        let sysfs = sysfs_root.join(&name);
        let Ok(uevent) = fs::read_to_string(sysfs.join("device/uevent")) else {
            continue;
        };
        let Some(identity) = parse_uevent(&uevent) else {
            continue;
        };
        let interface = match identity.bus {
            Bus::Usb => usb_interface_number(&sysfs),
            _ => None,
        };
        let node = Node {
            path: dev_root.join(&name),
            bus: identity.bus,
            vendor: identity.vendor,
            product: identity.product,
            interface,
            name: identity.name,
        };
        if filter.matches(&node) {
            nodes.push(node);
        }
    }
    Ok(nodes)
}

/// The `N` of `hidrawN`, to sort the nodes numerically; anything else sorts
/// last, by name.
fn node_number(name: &OsStr) -> Option<u32> {
    name.to_str()?.strip_prefix("hidraw")?.parse().ok()
}

/// What a HID device's uevent says about it.
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    bus: Bus,
    vendor: u16,
    product: u16,
    name: String,
}

/// Reads `HID_ID` and `HID_NAME` out of a HID device's uevent.
///
/// `HID_ID` is three hex fields, `%04X:%08X:%08X`. It is parsed field by
/// field rather than matched as text, so that a stray substring (the
/// `MODALIAS` line carries the same numbers) can never pass for a device, and
/// a line with a missing, malformed or extra field is no identity at all.
fn parse_uevent(uevent: &str) -> Option<Identity> {
    let field = |key: &str| {
        uevent
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
    };

    let mut ids = field("HID_ID")?.trim().split(':');
    let bus = u16::from_str_radix(ids.next()?, 16).ok()?;
    let vendor = u32::from_str_radix(ids.next()?, 16).ok()?;
    let product = u32::from_str_radix(ids.next()?, 16).ok()?;
    if ids.next().is_some() {
        return None;
    }
    Some(Identity {
        bus: Bus::from_raw(bus),
        vendor: u16::try_from(vendor).ok()?,
        product: u16::try_from(product).ok()?,
        name: field("HID_NAME").unwrap_or_default().trim().to_owned(),
    })
}

/// The USB interface number behind a hidraw node, read from sysfs.
///
/// The node's `device` symlink resolves to the HID device directory
/// (`0003:1532:00A4.0007`); the USB interface directory is the ancestor that
/// carries `bInterfaceNumber` (`3-2:1.N` under USB, but the attribute is what
/// defines it, not the name). The attribute is hex, as every USB descriptor
/// field in sysfs.
fn usb_interface_number(sysfs_node: &Path) -> Option<u8> {
    let device = fs::canonicalize(sysfs_node.join("device")).ok()?;
    let attribute = device
        .ancestors()
        .map(|dir| dir.join("bInterfaceNumber"))
        .find(|path| path.exists())?;
    let text = fs::read_to_string(attribute).ok()?;
    u8::from_str_radix(text.trim(), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uevent_is_parsed_field_by_field() {
        let uevent = "DRIVER=hid-generic\nHID_ID=0003:00001532:000000A4\n\
                      HID_NAME=Razer Razer Mouse Dock Pro\nHID_PHYS=usb-0000:00:14.0-3/input0\n\
                      MODALIAS=hid:b0003g0001v00001532p000000A4\n";
        assert_eq!(
            parse_uevent(uevent),
            Some(Identity {
                bus: Bus::Usb,
                vendor: 0x1532,
                product: 0x00a4,
                name: "Razer Razer Mouse Dock Pro".to_owned(),
            })
        );

        // Bluetooth and uhid devices carry the same IDs; the name is optional.
        let bluetooth = parse_uevent("HID_ID=0005:00001532:000000A4\n").unwrap();
        assert_eq!(bluetooth.bus, Bus::Bluetooth);
        assert_eq!(bluetooth.name, "");
        assert_eq!(
            parse_uevent("HID_ID=0006:00003329:00004B18\nHID_NAME=Audeze Maxwell\n")
                .unwrap()
                .bus,
            Bus::Virtual
        );
        assert_eq!(
            parse_uevent("HID_ID=0018:000004F3:00002D9F\n").unwrap().bus,
            Bus::I2c
        );
        assert_eq!(
            parse_uevent("HID_ID=0019:00000001:00000002\n").unwrap().bus,
            Bus::Other(0x19)
        );

        // No HID_ID line, a MODALIAS carrying the numbers, or a HID_ID with a
        // missing, malformed, extra or oversized field.
        assert_eq!(parse_uevent("DRIVER=hid-generic\n"), None);
        assert_eq!(
            parse_uevent("MODALIAS=hid:b0003g0001v00001532p000000A4\n"),
            None
        );
        assert_eq!(parse_uevent("HID_ID=0003:00001532\n"), None);
        assert_eq!(parse_uevent("HID_ID=0003:0000zz32:000000A4\n"), None);
        assert_eq!(parse_uevent("HID_ID=0003:00001532:000000A4:0000\n"), None);
        assert_eq!(parse_uevent("HID_ID=garbage\n"), None);
        assert_eq!(parse_uevent("HID_ID=0003:00011532:000000A4\n"), None);
    }

    #[test]
    fn bus_numbers_round_trip() {
        for raw in [0x0003, 0x0005, 0x0006, 0x0018, 0x0001, 0xffff] {
            assert_eq!(Bus::from_raw(raw).raw(), raw);
        }
        assert_eq!(Bus::from_raw(0x0003), Bus::Usb);
        assert_eq!(Bus::from_raw(0x0001), Bus::Other(1));
    }

    #[test]
    fn a_filter_matches_what_is_set() {
        let node = Node {
            path: PathBuf::from("/dev/hidraw3"),
            bus: Bus::Usb,
            vendor: 0x1532,
            product: 0x00a4,
            interface: Some(2),
            name: String::new(),
        };
        assert!(Filter::new().matches(&node));
        assert!(Filter::default().matches(&node));
        assert!(Filter::new().vendor(0x1532).product(0x00a4).matches(&node));
        assert!(Filter::new().bus(Bus::Usb).interface(2).matches(&node));
        assert!(!Filter::new().vendor(0x3329).matches(&node));
        assert!(!Filter::new().product(0x00a5).matches(&node));
        assert!(!Filter::new().bus(Bus::Bluetooth).matches(&node));
        assert!(!Filter::new().interface(0).matches(&node));

        // An interface filter never matches a node that has none.
        let bluetooth = Node {
            bus: Bus::Bluetooth,
            interface: None,
            ..node
        };
        assert!(Filter::new().vendor(0x1532).matches(&bluetooth));
        assert!(!Filter::new().interface(2).matches(&bluetooth));
    }

    /// A fake sysfs in a temporary directory, laid out like the kernel's:
    /// `class/hidraw/hidrawN/device` is a symlink into `devices/`, where the
    /// HID device directory carries the `uevent` and, under USB, the
    /// interface directory above it carries `bInterfaceNumber`.
    struct FakeSysfs {
        dir: tempfile::TempDir,
    }

    impl FakeSysfs {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir_all(dir.path().join("class/hidraw")).unwrap();
            Self { dir }
        }

        fn class_dir(&self) -> PathBuf {
            self.dir.path().join("class/hidraw")
        }

        fn link(&self, n: u32, hid_dir: &Path) {
            let hidraw_dir = self.class_dir().join(format!("hidraw{n}"));
            fs::create_dir_all(&hidraw_dir).unwrap();
            std::os::unix::fs::symlink(hid_dir, hidraw_dir.join("device")).unwrap();
        }

        fn uevent(hid_dir: &Path, bus: u16, vendor: u32, product: u32, name: &str) {
            fs::create_dir_all(hid_dir).unwrap();
            fs::write(
                hid_dir.join("uevent"),
                format!(
                    "DRIVER=hid-generic\nHID_ID={bus:04X}:{vendor:08X}:{product:08X}\n\
                     HID_NAME={name}\nHID_PHYS=whatever\n"
                ),
            )
            .unwrap();
        }

        /// `hidrawN` on USB interface `interface` of the device at `port`.
        fn add_usb(
            &self,
            n: u32,
            port: &str,
            interface: u8,
            vendor: u32,
            product: u32,
            name: &str,
        ) {
            let iface_dir = self.dir.path().join(format!(
                "devices/pci0000:00/0000:00:14.0/usb1/{port}/{port}:1.{interface}"
            ));
            let hid_dir = iface_dir.join(format!("0003:{vendor:04X}:{product:04X}.{n:04}"));
            Self::uevent(&hid_dir, 0x0003, vendor, product, name);
            fs::write(
                iface_dir.join("bInterfaceNumber"),
                format!("{interface:02x}\n"),
            )
            .unwrap();
            self.link(n, &hid_dir);
        }

        /// `hidrawN` on another bus: no USB interface anywhere above.
        fn add_other(&self, n: u32, bus: u16, vendor: u32, product: u32, name: &str) {
            let parent = match bus {
                0x0005 => "devices/pci0000:00/0000:00:14.0/usb1/1-9/bluetooth/hci0/hci0:256",
                0x0006 => "devices/virtual/misc/uhid",
                _ => "devices/platform/AMDI0010:00/i2c-0/i2c-ELAN0670:00",
            };
            let hid_dir = self
                .dir
                .path()
                .join(parent)
                .join(format!("{bus:04X}:{vendor:04X}:{product:04X}.{n:04}"));
            Self::uevent(&hid_dir, bus, vendor, product, name);
            self.link(n, &hid_dir);
        }
    }

    fn discover_fake(sysfs: &FakeSysfs, filter: &Filter) -> Vec<Node> {
        discover_in(&sysfs.class_dir(), Path::new("/dev"), filter).unwrap()
    }

    #[test]
    fn every_node_is_listed_with_its_identity_in_numeric_order() {
        let sysfs = FakeSysfs::new();
        // A composite USB device with three interfaces, interleaved with
        // lookalikes: the same IDs over Bluetooth, another product of the
        // same vendor, a uhid device with the same IDs, an I2C touchpad.
        sysfs.add_usb(0, "1-3", 1, 0x1532, 0x00a4, "Razer Razer Mouse Dock Pro");
        sysfs.add_other(1, 0x0005, 0x1532, 0x00a4, "Razer Mouse");
        sysfs.add_usb(2, "1-5", 0, 0x1532, 0x02b3, "Razer Keyboard");
        sysfs.add_usb(3, "1-3", 0, 0x1532, 0x00a4, "Razer Razer Mouse Dock Pro");
        sysfs.add_usb(10, "1-3", 2, 0x1532, 0x00a4, "Razer Razer Mouse Dock Pro");
        sysfs.add_other(5, 0x0006, 0x1532, 0x00a4, "razerd virtual battery");
        sysfs.add_other(6, 0x0018, 0x04f3, 0x2d9f, "ELAN0670:00 04F3:2D9F");

        let all = discover_fake(&sysfs, &Filter::new());
        let summary: Vec<_> = all
            .iter()
            .map(|node| {
                (
                    node.path.to_str().unwrap(),
                    node.bus,
                    node.vendor,
                    node.product,
                    node.interface,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("/dev/hidraw0", Bus::Usb, 0x1532, 0x00a4, Some(1)),
                ("/dev/hidraw1", Bus::Bluetooth, 0x1532, 0x00a4, None),
                ("/dev/hidraw2", Bus::Usb, 0x1532, 0x02b3, Some(0)),
                ("/dev/hidraw3", Bus::Usb, 0x1532, 0x00a4, Some(0)),
                ("/dev/hidraw5", Bus::Virtual, 0x1532, 0x00a4, None),
                ("/dev/hidraw6", Bus::I2c, 0x04f3, 0x2d9f, None),
                ("/dev/hidraw10", Bus::Usb, 0x1532, 0x00a4, Some(2)),
            ]
        );
        assert_eq!(all[0].name, "Razer Razer Mouse Dock Pro");
        assert_eq!(all[4].name, "razerd virtual battery");
    }

    #[test]
    fn a_filter_picks_the_usb_interface_among_the_lookalikes() {
        let sysfs = FakeSysfs::new();
        sysfs.add_usb(0, "1-3", 1, 0x1532, 0x00a4, "dock");
        sysfs.add_other(1, 0x0005, 0x1532, 0x00a4, "mouse");
        sysfs.add_usb(3, "1-3", 0, 0x1532, 0x00a4, "dock");
        sysfs.add_usb(4, "1-3", 2, 0x1532, 0x00a4, "dock");
        sysfs.add_other(5, 0x0006, 0x1532, 0x00a4, "battery");

        let dock = Filter::new().bus(Bus::Usb).vendor(0x1532).product(0x00a4);
        let found = discover_fake(&sysfs, &dock.interface(0));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, Path::new("/dev/hidraw3"));
        let found = discover_fake(&sysfs, &dock.interface(2));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, Path::new("/dev/hidraw4"));

        // All the dock's interfaces, in interface order once sorted.
        let mut found = discover_fake(&sysfs, &dock);
        found.sort_by_key(|node| node.interface);
        let interfaces: Vec<_> = found.iter().map(|node| node.interface).collect();
        assert_eq!(interfaces, [Some(0), Some(1), Some(2)]);

        // Right IDs, no such interface; and the IDs on the other buses.
        assert_eq!(discover_fake(&sysfs, &dock.interface(3)), []);
        let virtual_ = discover_fake(&sysfs, &Filter::new().bus(Bus::Virtual));
        assert_eq!(virtual_.len(), 1);
        assert_eq!(virtual_[0].path, Path::new("/dev/hidraw5"));
        assert_eq!(virtual_[0].interface, None);
    }

    #[test]
    fn the_device_root_names_the_nodes() {
        let sysfs = FakeSysfs::new();
        sysfs.add_usb(
            7,
            "1-3",
            0,
            0x3329,
            0x4b18,
            "Audeze LLC Audeze Maxwell XBOX Dongle",
        );
        let found = discover_in(
            &sysfs.class_dir(),
            Path::new("/run/fake-dev"),
            &Filter::new(),
        )
        .unwrap();
        assert_eq!(found[0].path, Path::new("/run/fake-dev/hidraw7"));
    }

    #[test]
    fn an_entry_without_a_readable_uevent_or_identity_is_skipped() {
        let sysfs = FakeSysfs::new();
        // A node whose device vanished between the listing and the read.
        fs::create_dir_all(sysfs.class_dir().join("hidraw0")).unwrap();
        // A node whose uevent has no HID_ID.
        fs::create_dir_all(sysfs.class_dir().join("hidraw1/device")).unwrap();
        fs::write(
            sysfs.class_dir().join("hidraw1/device/uevent"),
            "DRIVER=hid-generic\n",
        )
        .unwrap();
        sysfs.add_usb(2, "1-3", 0, 0x1532, 0x00a4, "dock");

        let found = discover_fake(&sysfs, &Filter::new());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, Path::new("/dev/hidraw2"));
    }

    #[test]
    fn a_usb_node_without_an_interface_attribute_has_no_interface() {
        // A USB HID device whose sysfs tree lacks `bInterfaceNumber` (a
        // driver that does not go through usbhid): the bus says USB, the
        // interface is unknown rather than guessed.
        let sysfs = FakeSysfs::new();
        sysfs.add_other(0, 0x0003, 0x1532, 0x00a4, "odd");
        let found = discover_fake(&sysfs, &Filter::new());
        assert_eq!(found[0].bus, Bus::Usb);
        assert_eq!(found[0].interface, None);
    }

    #[test]
    fn an_unreadable_class_directory_is_an_error() {
        let err = discover_in(
            Path::new("/nonexistent/hidraw"),
            Path::new("/dev"),
            &Filter::new(),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
