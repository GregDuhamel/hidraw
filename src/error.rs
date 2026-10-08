//! Telling a device that went away from a transfer that merely failed.
//!
//! The kernel's two errnos for a gone device, and why `EIO` has to be
//! settled where it is produced, are documented on [`DeviceGone`]; the
//! verdict is [`is_gone`].

use std::error::Error;
use std::fmt;
use std::io;

/// `ENODEV`, the errno the ioctls and `write(2)` answer for a gone device.
const ENODEV: i32 = rustix::io::Errno::NODEV.raw_os_error();

/// `EIO`, the errno `read(2)` answers for a gone device.
const EIO: i32 = rustix::io::Errno::IO.raw_os_error();

/// The error [`Device::read`](crate::Device::read) returns for a device that
/// went away: an `EIO` from `read(2)`, which on that path means the node is
/// dead.
///
/// The kernel does not make a gone device easy to tell from a transfer that
/// merely failed: the ioctls and `write(2)` answer `ENODEV` once the device
/// is gone (`hidraw_ioctl`, `hidraw_send_report`: `!hidraw->exist`), but
/// `read(2)` answers `EIO` instead (`hidraw_read`, same condition) - the very
/// errno a transfer that failed for a transient reason also produces on the
/// ioctl path. So `EIO` means two different things depending on where it
/// came from, and `read` settles it at the source by wrapping its `EIO` in
/// this type, while an `EIO` from an ioctl is returned untouched and stays
/// transient.
///
/// It is the inner error of an [`io::Error`] of kind
/// [`io::ErrorKind::NotConnected`]; [`is_gone`] recognises it, and so does
/// any walk of an error chain that downcasts to it. The original `EIO` is
/// its [`source`](Error::source), for whoever prints the chain.
#[derive(Debug)]
pub struct DeviceGone {
    source: io::Error,
}

impl fmt::Display for DeviceGone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "hidraw device gone: {}", self.source)
    }
}

impl Error for DeviceGone {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Wraps an `EIO` from `read(2)` in a [`DeviceGone`]; any other error passes
/// through untouched.
pub(crate) fn mark_gone_on_read(err: io::Error) -> io::Error {
    if err.raw_os_error() == Some(EIO) {
        io::Error::new(io::ErrorKind::NotConnected, DeviceGone { source: err })
    } else {
        err
    }
}

/// Did this operation fail because the device behind the node is gone?
///
/// `true` for an `ENODEV`, which the ioctls and [`Device::write`] answer
/// for an unplugged device, and for the [`DeviceGone`] that [`Device::read`]
/// makes of its `EIO`. `false` for everything else, including an `EIO`,
/// `EPIPE` or `ETIMEDOUT` from an ioctl - a control transfer that failed
/// because the device is asleep, out of range, or busy - and a timeout, which
/// carries no errno at all.
///
/// A daemon that wraps its errors (in `anyhow`, say) walks the chain and
/// applies this to the first `io::Error` it finds.
///
/// [`Device::read`]: crate::Device::read
/// [`Device::write`]: crate::Device::write
#[must_use]
pub fn is_gone(err: &io::Error) -> bool {
    err.raw_os_error() == Some(ENODEV)
        || err
            .get_ref()
            .is_some_and(<dyn std::error::Error + Send + Sync>::is::<DeviceGone>)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enodev_is_gone_and_other_errnos_are_not() {
        assert!(is_gone(&io::Error::from_raw_os_error(ENODEV)));
        // A stalled or timed-out control transfer is transient.
        let epipe = rustix::io::Errno::PIPE.raw_os_error();
        let etimedout = rustix::io::Errno::TIMEDOUT.raw_os_error();
        assert!(!is_gone(&io::Error::from_raw_os_error(epipe)));
        assert!(!is_gone(&io::Error::from_raw_os_error(etimedout)));
        // A timeout carries no errno at all.
        assert!(!is_gone(&io::Error::from(io::ErrorKind::TimedOut)));
        assert!(!is_gone(&io::Error::other("the device did not answer")));
    }

    /// `read()` on an unplugged hidraw fails with EIO, not ENODEV; the same
    /// errno from an ioctl is a transfer error and must stay transient.
    #[test]
    fn eio_is_gone_on_read_but_not_on_ioctl() {
        let on_ioctl = io::Error::from_raw_os_error(EIO);
        assert!(!is_gone(&on_ioctl));

        let on_read = mark_gone_on_read(io::Error::from_raw_os_error(EIO));
        assert!(is_gone(&on_read));
        assert_eq!(on_read.kind(), io::ErrorKind::NotConnected);
        assert_eq!(
            on_read.to_string(),
            "hidraw device gone: Input/output error (os error 5)"
        );
        // The errno stays reachable for whoever prints the chain.
        let source = on_read.get_ref().unwrap().source().unwrap();
        let errno = source.downcast_ref::<io::Error>().unwrap().raw_os_error();
        assert_eq!(errno, Some(EIO));

        // Anything else from read() passes through as it is.
        let eagain = rustix::io::Errno::AGAIN.raw_os_error();
        let other = mark_gone_on_read(io::Error::from_raw_os_error(eagain));
        assert_eq!(other.raw_os_error(), Some(eagain));
        assert!(!is_gone(&other));
    }
}
