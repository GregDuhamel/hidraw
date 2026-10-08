//! An open hidraw node.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::mark_gone_on_read;
use crate::ioctl;
use crate::poll::{PollFd, PollFlags, poll};

/// How many input reports the kernel queues per open handle
/// (`HIDRAW_BUFFER_SIZE` in `linux/hidraw.h`), dropping the oldest beyond
/// that.
pub const INPUT_QUEUE_LEN: usize = 64;

/// An open `/dev/hidrawN`.
///
/// # Buffers
///
/// Every report buffer, in or out, follows the kernel's convention: byte 0 is
/// the report ID, the report follows. A device whose descriptor declares no
/// report IDs takes a `0` there, which the kernel strips before the transfer
/// (`SET`) or leaves in place in front of what it fetched (`GET`). The
/// buffer for a `GET` must be as large as the report plus one, or the report
/// is cut.
///
/// # Blocking
///
/// The node is opened blocking: [`read`](Self::read) waits for a report, as
/// a daemon that listens for a device's spontaneous input wants. Every other
/// read goes through `poll(2)` first - [`read_timeout`](Self::read_timeout),
/// [`drain`](Self::drain) - so nothing here blocks beyond the timeout it was
/// given. The feature and input ioctls are synchronous control transfers and
/// return when the device has answered, or the USB stack gave up.
///
/// # Errors
///
/// Every method returns the kernel's `io::Error`. Which of them mean the
/// device is gone for good is the business of [`is_gone`](crate::is_gone).
#[derive(Debug)]
pub struct Device {
    file: File,
    path: PathBuf,
}

impl AsFd for Device {
    /// The node's descriptor, for a daemon that polls it next to other
    /// descriptors (see [`poll`]).
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl Device {
    /// Opens `path` for reading and writing.
    ///
    /// The descriptor is `O_RDWR | O_CLOEXEC`, blocking (see the [type
    /// documentation](Self#blocking)). Feature reports need read *and* write
    /// access to the node, whatever their direction, since `ioctl(2)` takes
    /// either but the udev rules that grant a daemon access usually grant
    /// both at once.
    ///
    /// # Errors
    ///
    /// `PermissionDenied` is what a missing udev rule looks like, and
    /// `NotFound` what an unplugged device looks like when the path was
    /// obtained earlier.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            file,
            path: path.to_owned(),
        })
    }

    /// Wraps a descriptor opened elsewhere - passed down by systemd's
    /// `OpenFile=`, say - with the path it is known by, for messages.
    ///
    /// The descriptor must be a hidraw node opened for reading and writing,
    /// in blocking mode; nothing checks it, the first operation fails
    /// otherwise.
    pub fn from_fd(fd: impl Into<OwnedFd>, path: impl Into<PathBuf>) -> Self {
        Self {
            file: File::from(fd.into()),
            path: path.into(),
        }
    }

    /// The `/dev/hidrawN` this handle was opened on.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sends a feature report (`HIDIOCSFEATURE`: a `SET_REPORT` control
    /// transfer of type feature).
    ///
    /// `report[0]` is the report ID, `0` for a device without.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for an empty or oversized buffer; otherwise the
    /// kernel's errno: `ENODEV` for a device that is gone, `EIO`, `EPIPE` or
    /// `ETIMEDOUT` for a transfer the device did not take (it is asleep, out
    /// of range, or rejected the request), which is transient.
    pub fn set_feature(&self, report: &[u8]) -> io::Result<()> {
        // The opcode declares the buffer read *and* written although the
        // kernel only reads it for a SET: a scratch copy keeps the `&[u8]`
        // signature honest rather than casting the constness away.
        let mut buf = report.to_vec();
        ioctl::report(&self.file, ioctl::SET_FEATURE, &mut buf)?;
        Ok(())
    }

    /// Fetches a feature report (`HIDIOCGFEATURE`: a `GET_REPORT` control
    /// transfer of type feature) into `report`, and returns how many bytes
    /// the kernel wrote, report ID included.
    ///
    /// On entry `report[0]` is the ID of the report to fetch, `0` for a
    /// device without; on return the report follows it. A device with
    /// numbered reports answers at most the size of that report plus one;
    /// one without answers at most `report.len()`.
    ///
    /// # Errors
    ///
    /// As [`set_feature`](Self::set_feature).
    pub fn get_feature(&self, report: &mut [u8]) -> io::Result<usize> {
        ioctl::report(&self.file, ioctl::GET_FEATURE, report)
    }

    /// Fetches the current value of an input report (`HIDIOCGINPUT`: a
    /// `GET_REPORT` control transfer of type input) into `report`, and
    /// returns how many bytes the kernel wrote, report ID included.
    ///
    /// This is for a device that answers requests in a report it never
    /// pushes on the interrupt endpoint - a plain [`read`](Self::read) sees
    /// nothing - so the answer has to be fetched. The buffer convention is
    /// that of [`get_feature`](Self::get_feature).
    ///
    /// # Errors
    ///
    /// As [`set_feature`](Self::set_feature); plus `ENOTTY` on a kernel
    /// before 5.11, which does not have this ioctl.
    pub fn get_input(&self, report: &mut [u8]) -> io::Result<usize> {
        ioctl::report(&self.file, ioctl::GET_INPUT, report)
    }

    /// Sends an output report through `write(2)`: on USB, an interrupt-out
    /// transfer when the interface has such an endpoint, a `SET_REPORT`
    /// control transfer otherwise.
    ///
    /// `report[0]` is the report ID, `0` for a device without (the kernel
    /// strips it). One `write(2)` is one report: hidraw never writes a report
    /// partially, so a short count is reported as an error rather than
    /// retried, which would send the tail as a second, broken report. A
    /// signal that interrupts the call before anything was sent is retried.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for an empty buffer, `WriteZero` for a short write;
    /// otherwise the kernel's errno, `ENODEV` for a device that is gone.
    pub fn write(&self, report: &[u8]) -> io::Result<()> {
        if report.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a hidraw report starts with its report ID; an empty buffer has none",
            ));
        }
        let written = loop {
            match (&self.file).write(report) {
                Ok(written) => break written,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        };
        if written != report.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!(
                    "{}: wrote {written} of {} report bytes",
                    self.path.display(),
                    report.len()
                ),
            ));
        }
        Ok(())
    }

    /// Reads one input report into `buf` and returns its length, blocking
    /// until the device sends one.
    ///
    /// The kernel hands over one report per `read(2)`, report ID first when
    /// the device has them, and cuts it to `buf.len()` when it is larger: a
    /// too-small buffer loses the tail silently. A signal that interrupts the
    /// wait is retried; use [`read_timeout`](Self::read_timeout) or
    /// [`wait_readable`](Self::wait_readable) to bound the wait.
    ///
    /// # Errors
    ///
    /// A [`DeviceGone`](crate::DeviceGone) when the device went away: the
    /// kernel answers `EIO` on this path, which [`is_gone`](crate::is_gone)
    /// would otherwise mistake for a transient transfer error, so it is
    /// wrapped here. Other errors pass through.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match (&self.file).read(buf) {
                Ok(len) => return Ok(len),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(mark_gone_on_read(err)),
            }
        }
    }

    /// Waits until an input report is queued, or `timeout` elapses; `true`
    /// means [`read`](Self::read) will not block.
    ///
    /// One `poll(POLLIN)`, taken up again with whatever time is left when a
    /// signal cuts it short, so a `Duration::ZERO` asks whether a report is
    /// already there. The kernel rounds the timeout up to its clock, never
    /// down (see [`poll`]). A device that went away is reported readable too
    /// (`hidraw_poll` answers `POLLERR | POLLHUP`), so that the read that
    /// follows meets the error and reports it.
    ///
    /// # Errors
    ///
    /// Those of `poll(2)`, which are not expected on a valid descriptor.
    pub fn wait_readable(&self, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut remaining = timeout;
        loop {
            let mut fds = [PollFd::new(&self.file, PollFlags::IN)];
            match poll(&mut fds, remaining) {
                Ok(ready) => return Ok(ready > 0),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {
                    remaining = deadline.saturating_duration_since(Instant::now());
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Reads one input report if one arrives within `timeout`: `Some(len)`
    /// as [`read`](Self::read) would return it, `None` on timeout.
    ///
    /// # Errors
    ///
    /// Those of [`wait_readable`](Self::wait_readable) and [`read`](Self::read).
    pub fn read_timeout(&self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
        if !self.wait_readable(timeout)? {
            return Ok(None);
        }
        self.read(buf).map(Some)
    }

    /// Discards the input reports queued on this handle, without blocking,
    /// and returns how many were thrown away.
    ///
    /// For a daemon that sends a request and waits for the reply with
    /// [`read_timeout`](Self::read_timeout): whatever the device sent before
    /// is not that reply. The kernel keeps at most [`INPUT_QUEUE_LEN`]
    /// reports per open handle, so one pass of that many reads empties what
    /// was queued when the call began; a report that arrives during the pass
    /// may be left, which a return value of [`INPUT_QUEUE_LEN`] hints at.
    ///
    /// # Errors
    ///
    /// Those of [`wait_readable`](Self::wait_readable) and [`read`](Self::read).
    pub fn drain(&self) -> io::Result<usize> {
        // The read cuts a report longer than this; it is thrown away anyway.
        let mut scratch = [0u8; 64];
        let mut discarded = 0;
        while discarded < INPUT_QUEUE_LEN && self.wait_readable(Duration::ZERO)? {
            self.read(&mut scratch)?;
            discarded += 1;
        }
        Ok(discarded)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixDatagram;

    use super::*;

    /// A datagram socket pair stands in for the node: like hidraw, it
    /// delivers one message per `read(2)` and cuts a message longer than the
    /// buffer.
    fn fake() -> (Device, UnixDatagram) {
        let (node, peer) = UnixDatagram::pair().unwrap();
        (Device::from_fd(node, "/dev/hidraw-fake"), peer)
    }

    #[test]
    fn the_path_is_what_it_was_opened_as() {
        let (device, _peer) = fake();
        assert_eq!(device.path(), Path::new("/dev/hidraw-fake"));
    }

    #[test]
    fn a_report_is_waited_for_then_read() {
        let (device, peer) = fake();
        let mut buf = [0u8; 8];
        assert!(!device.wait_readable(Duration::ZERO).unwrap());
        assert!(!device.wait_readable(Duration::from_millis(5)).unwrap());
        assert_eq!(device.read_timeout(&mut buf, Duration::ZERO).unwrap(), None);

        peer.send(&[1, 2, 3]).unwrap();
        assert!(device.wait_readable(Duration::ZERO).unwrap());
        assert_eq!(
            device
                .read_timeout(&mut buf, Duration::from_secs(5))
                .unwrap(),
            Some(3)
        );
        assert_eq!(&buf[..3], &[1, 2, 3]);

        peer.send(&[4]).unwrap();
        assert_eq!(device.read(&mut buf).unwrap(), 1);
        assert_eq!(buf[0], 4);

        // A report longer than the buffer is cut, not split over two reads.
        peer.send(&[0; 20]).unwrap();
        assert_eq!(device.read(&mut buf).unwrap(), 8);
        assert!(!device.wait_readable(Duration::ZERO).unwrap());
    }

    #[test]
    fn a_write_is_one_report() {
        let (device, peer) = fake();
        device.write(&[0, 0xaa, 0xbb]).unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(peer.recv(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], &[0, 0xaa, 0xbb]);

        let err = device.write(&[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn draining_throws_away_what_is_queued_and_counts_it() {
        let (device, peer) = fake();
        assert_eq!(device.drain().unwrap(), 0);
        for i in 0..5u8 {
            peer.send(&[i; 100]).unwrap(); // longer than the scratch buffer
        }
        assert_eq!(device.drain().unwrap(), 5);
        assert!(!device.wait_readable(Duration::ZERO).unwrap());

        // One pass is bounded by the kernel's queue length.
        for _ in 0..INPUT_QUEUE_LEN + 3 {
            peer.send(&[0]).unwrap();
        }
        assert_eq!(device.drain().unwrap(), INPUT_QUEUE_LEN);
        assert_eq!(device.drain().unwrap(), 3);
    }

    #[test]
    fn a_gone_peer_is_reported_through_the_read() {
        // Not the kernel's EIO - a socket cannot produce it - but the same
        // shape: poll says readable (HUP), the read that follows fails or
        // ends, and nothing blocks. A stream pair: a datagram pair raises no
        // HUP when its peer closes.
        let (node, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let device = Device::from_fd(node, "/dev/hidraw-fake");
        drop(peer);
        assert!(device.wait_readable(Duration::from_secs(5)).unwrap());
        let mut buf = [0u8; 8];
        match device.read(&mut buf) {
            Ok(0) | Err(_) => {}
            Ok(len) => panic!("read {len} bytes from a closed peer"),
        }
    }

    /// The real thing, read-only: lists what sysfs has and opens what the
    /// udev rules allow, without sending anything to any device. Run with
    /// `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the machine's hidraw nodes"]
    fn hardware_nodes_are_listed_and_opened() {
        let nodes = crate::discover(&crate::Filter::new()).unwrap();
        for node in &nodes {
            println!("{node:?}");
            match Device::open(&node.path) {
                Ok(device) => {
                    // Whatever is queued belongs to this handle alone.
                    let pending = device.wait_readable(Duration::ZERO).unwrap();
                    println!("  opened; input pending: {pending}");
                }
                Err(err) => {
                    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
                    println!("  not openable: {err}");
                }
            }
        }
    }
}
