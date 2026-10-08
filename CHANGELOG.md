# Changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions are the git
tags consumers pin (`tag = "vX.Y.Z"`).

## [Unreleased]

## [0.1.0] - 2026-10-08

The hidraw transport that razerd, thermaltaked and HeadsetBatteryIndicator
each carried a copy of, as one crate. Linux only, `rustix` only: the single
`unsafe` is the report ioctls, in one module.

### Added

- Discovery through sysfs: `discover(&Filter)` and `discover_in(sysfs_root,
  dev_root, &Filter)` list the `Node`s under `/sys/class/hidraw` in numeric
  order, each with its `Bus` (`Usb`, `Bluetooth`, `Virtual`, `I2c`,
  `Other(u16)`), vendor and product from the uevent's `HID_ID` (parsed field
  by field), its name from `HID_NAME`, and for a USB device the
  `bInterfaceNumber` of the interface above it. Entries whose uevent cannot
  be read, or has no well-formed `HID_ID`, are skipped. `Filter` is a
  builder (`bus`, `vendor`, `product`, `interface`) whose unset fields match
  anything; `Node::open` opens the node.
- `Device`: `open(path)` (`O_RDWR | O_CLOEXEC`, blocking), `from_fd(fd,
  path)` for a descriptor passed down by systemd, `path()`, `AsFd`.
- Feature and input reports: `set_feature`, `get_feature` and `get_input`
  (`HIDIOCSFEATURE`, `HIDIOCGFEATURE`, `HIDIOCGINPUT`), with the report ID
  in byte 0 of every buffer as the kernel wants it, over `rustix::ioctl`
  with the length encoded in the opcode. The `_IOC` encoding is pinned by a
  regression test against the kernel's values.
- Output and input reports: `write` (one `write(2)`, a short write is an
  error rather than a retry, `EINTR` retried), `read` (one report, `EINTR`
  retried), `wait_readable(timeout)` (`poll(POLLIN)`, taken up again with
  the time left after a signal), `read_timeout(buf, timeout)` and `drain()`,
  which throws away up to `INPUT_QUEUE_LEN` (64) queued reports without
  blocking and says how many.
- `is_gone(&io::Error)`: `true` for `ENODEV` anywhere, and for the `EIO`
  that `read` wraps in a `DeviceGone`, since `read(2)` answers `EIO` for an
  unplugged node while the same errno from an ioctl is a transient transfer
  failure and stays `false`.
- `poll(fds, timeout)` with `PollFd` and `PollFlags` re-exported, for a daemon
  that watches the node next to other descriptors; `Interrupted` is passed
  through, as a daemon's sleep wants.

[Unreleased]: https://github.com/GregDuhamel/hidraw/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/GregDuhamel/hidraw/releases/tag/v0.1.0
