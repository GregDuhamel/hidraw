//! A one-function wrapper over `poll(2)`, speaking [`Duration`].
//!
//! [`Device::wait_readable`](crate::Device::wait_readable) is built on it,
//! and it is public for daemons that watch a hidraw node next to other
//! descriptors - a uhid battery, a socket, a signal pipe - so they do not have
//! to write the same ten lines. [`PollFd`] and [`PollFlags`] are re-exported
//! so that such a daemon need not depend on `rustix` itself.
//!
//! It is the same function as `uhid_battery::poll::poll`, duplicated so that
//! neither crate depends on the other.

use std::io;
use std::time::Duration;

use rustix::event::Timespec;
pub use rustix::event::{PollFd, PollFlags};

/// Waits until one of `fds` is ready, or `timeout` elapses.
///
/// Returns the number of ready descriptors, which is `0` on timeout. The
/// timeout is passed to the kernel at nanosecond resolution (`ppoll(2)`),
/// which rounds it *up* to its clock: a sub-millisecond timeout sleeps, it
/// does not spin. A timeout too long for the kernel's clock is as good as
/// none.
///
/// # Errors
///
/// Propagates `poll(2)` failures, including [`io::ErrorKind::Interrupted`]
/// when a signal arrives: `poll(2)` is never restarted after a handler ran,
/// whatever `SA_RESTART` says, which is what makes it the right way for a
/// daemon to sleep. A caller that wants to keep waiting recomputes the time
/// left and calls again, as [`Device::wait_readable`] does.
///
/// [`Device::wait_readable`]: crate::Device::wait_readable
pub fn poll(fds: &mut [PollFd<'_>], timeout: Duration) -> io::Result<usize> {
    let timeout = Timespec {
        tv_sec: timeout.as_secs().try_into().unwrap_or(i64::MAX),
        tv_nsec: timeout.subsec_nanos().into(),
    };
    rustix::event::poll(fds, Some(&timeout)).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixDatagram;

    use super::*;

    #[test]
    fn a_readable_descriptor_is_reported_and_a_timeout_is_zero() {
        let (left, right) = UnixDatagram::pair().unwrap();
        let mut fds = [PollFd::new(&left, PollFlags::IN)];
        assert_eq!(poll(&mut fds, Duration::from_millis(10)).unwrap(), 0);
        assert_eq!(poll(&mut fds, Duration::ZERO).unwrap(), 0);

        right.send(b"x").unwrap();
        assert_eq!(poll(&mut fds, Duration::from_secs(5)).unwrap(), 1);
        assert!(fds[0].revents().contains(PollFlags::IN));
        // A zero timeout still reports what is already there.
        assert_eq!(poll(&mut fds, Duration::ZERO).unwrap(), 1);
    }

    #[test]
    fn a_timeout_beyond_the_clock_is_accepted() {
        let (left, right) = UnixDatagram::pair().unwrap();
        right.send(b"x").unwrap();
        let mut fds = [PollFd::new(&left, PollFlags::IN)];
        assert_eq!(poll(&mut fds, Duration::MAX).unwrap(), 1);
    }
}
