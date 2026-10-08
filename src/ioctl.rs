//! The hidraw report ioctls, and the only `unsafe` in the crate.
//!
//! `linux/hidraw.h` defines the report ioctls with the buffer length encoded
//! in the opcode itself:
//!
//! ```text
//! #define HIDIOCSFEATURE(len)  _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x06, len)
//! #define HIDIOCGFEATURE(len)  _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x07, len)
//! #define HIDIOCGINPUT(len)    _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x0A, len)
//! ```
//!
//! so the opcode is a different number for every buffer size, and the
//! `rustix::ioctl` patterns built on a `const` opcode (`Updater`, `Getter`)
//! cannot express it. [`ReportIoctl`] implements [`rustix::ioctl::Ioctl`]
//! directly, computing the opcode from the buffer it is given.
//!
//! The buffer convention is the kernel's: byte 0 is the report ID (`0` for a
//! device whose descriptor declares none), the report follows. On a `GET`
//! the ID says which report to fetch and the kernel writes the report behind
//! it; on a `SET` the kernel strips the ID when the device has none, and
//! issues the control transfer.

use std::io;
use std::os::fd::AsFd;

use rustix::ioctl::{Direction, Ioctl, IoctlOutput, Opcode, opcode};

/// The `_IOC` group of every hidraw ioctl.
const GROUP: u8 = b'H';

/// `_IOC_NR(HIDIOCSFEATURE(0))`.
pub(crate) const SET_FEATURE: u8 = 0x06;
/// `_IOC_NR(HIDIOCGFEATURE(0))`.
pub(crate) const GET_FEATURE: u8 = 0x07;
/// `_IOC_NR(HIDIOCGINPUT(0))`, Linux 5.11 and later.
pub(crate) const GET_INPUT: u8 = 0x0a;

/// The widest buffer an `_IOC` opcode can describe: `_IOC_SIZEBITS` is 14 on
/// every architecture but mips, powerpc and sparc, where it is 13. The
/// kernel's own ceiling (`HID_MAX_BUFFER_SIZE`, 16384 since Linux 6.3, 4096
/// before) is enforced by the kernel with `EINVAL`.
#[cfg(not(any(
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
const MAX_LEN: usize = (1 << 14) - 1;
#[cfg(any(
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "sparc",
    target_arch = "sparc64"
))]
const MAX_LEN: usize = (1 << 13) - 1;

/// `_IOC(_IOC_WRITE|_IOC_READ, 'H', number, len)`.
///
/// `len` must fit the opcode's size field: `from_components` would silently
/// mask a wider value into a different, smaller ioctl, so it is checked
/// before.
pub(crate) fn report_opcode(number: u8, len: usize) -> io::Result<Opcode> {
    if len == 0 || len > MAX_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a hidraw report buffer holds 1 to {MAX_LEN} bytes, not {len}"),
        ));
    }
    Ok(opcode::from_components(
        Direction::ReadWrite,
        GROUP,
        number,
        len,
    ))
}

/// One report ioctl over `buf`, whose length is part of the opcode.
struct ReportIoctl<'a> {
    opcode: Opcode,
    buf: &'a mut [u8],
}

// SAFETY: the opcode is one of the three `_IOWR('H', nr, len)` report ioctls,
// with `len` the exact length of `buf`; the kernel reads at most `len` bytes
// from, and writes at most `len` bytes into, the buffer it is given (it copies
// through `copy_from_user`/`copy_to_user` with that bound, `hidraw_ioctl` in
// `drivers/hid/hidraw.c`). `buf` is exclusively borrowed for the call, so the
// kernel's writes are seen by nobody else, and the ioctl's return value is
// the byte count it transferred, which `output_from_ptr` passes through
// without touching the buffer.
#[expect(
    unsafe_code,
    reason = "the hidraw ioctls are the crate's only kernel ABI"
)]
unsafe impl Ioctl for ReportIoctl<'_> {
    type Output = usize;

    const IS_MUTATING: bool = true;

    fn opcode(&self) -> Opcode {
        self.opcode
    }

    fn as_ptr(&mut self) -> *mut std::ffi::c_void {
        self.buf.as_mut_ptr().cast()
    }

    unsafe fn output_from_ptr(
        out: IoctlOutput,
        _extract_output: *mut std::ffi::c_void,
    ) -> rustix::io::Result<Self::Output> {
        // The kernel returns the transferred length; a negative value never
        // reaches here, `ioctl` has already turned it into an `Errno`.
        usize::try_from(out).map_err(|_| rustix::io::Errno::INVAL)
    }
}

/// Issues `_IOWR('H', number, buf.len())` over `buf` and returns the byte
/// count the kernel reports.
///
/// # Errors
///
/// `InvalidInput` for an empty or oversized buffer; otherwise the kernel's
/// errno, among which `ENODEV` for a device that is gone, `EIO`, `EPIPE` or
/// `ETIMEDOUT` for a control transfer that failed (the device is asleep, out
/// of range, or rejected the request), and `ENOTTY` for an ioctl this kernel
/// does not have.
pub(crate) fn report(fd: &impl AsFd, number: u8, buf: &mut [u8]) -> io::Result<usize> {
    let opcode = report_opcode(number, buf.len())?;
    // SAFETY: see the `Ioctl` impl above; `buf` is exclusively borrowed for
    // the duration of the call, and the opcode carries its exact length.
    #[expect(
        unsafe_code,
        reason = "the hidraw ioctls are the crate's only kernel ABI"
    )]
    let transferred = unsafe { rustix::ioctl::ioctl(fd, ReportIoctl { opcode, buf })? };
    Ok(transferred)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test against the kernel's encoding, as a C program
    /// including `linux/hidraw.h` prints it on x86-64: a 91-byte buffer
    /// (90 bytes of Razer report behind the report ID) and the 62-byte
    /// Audeze frame.
    #[cfg(not(any(
        target_arch = "mips",
        target_arch = "mips32r6",
        target_arch = "mips64",
        target_arch = "mips64r6",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "sparc",
        target_arch = "sparc64"
    )))]
    #[test]
    fn opcodes_match_the_kernel_encoding() {
        assert_eq!(report_opcode(SET_FEATURE, 91).unwrap(), 0xC05B_4806);
        assert_eq!(report_opcode(GET_FEATURE, 91).unwrap(), 0xC05B_4807);
        assert_eq!(report_opcode(GET_INPUT, 62).unwrap(), 0xC03E_480A);
        // The largest encodable buffer, and the smallest.
        assert_eq!(report_opcode(GET_FEATURE, 0x3fff).unwrap(), 0xFFFF_4807);
        assert_eq!(report_opcode(GET_FEATURE, 1).unwrap(), 0xC001_4807);
    }

    #[test]
    fn a_buffer_the_opcode_cannot_describe_is_refused() {
        for len in [0, MAX_LEN + 1, usize::MAX] {
            let err = report_opcode(GET_FEATURE, len).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{len}: {err}");
        }
        assert!(report_opcode(GET_FEATURE, MAX_LEN).is_ok());
    }

    #[test]
    fn a_descriptor_that_is_not_a_hidraw_rejects_the_ioctl() {
        // `/dev/null` has no ioctls: the kernel answers ENOTTY, and the
        // error surfaces as an io::Error with that errno.
        let null = std::fs::File::open("/dev/null").unwrap();
        let mut buf = [0u8; 8];
        let err = report(&null, GET_FEATURE, &mut buf).unwrap_err();
        assert_eq!(
            err.raw_os_error(),
            Some(rustix::io::Errno::NOTTY.raw_os_error())
        );
    }
}
