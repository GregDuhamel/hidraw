//! Linux hidraw for daemons: find a device's hidraw nodes through sysfs, talk
//! to it with feature and input reports, wait on it with a timeout, and tell a
//! device that went away from a transient error.
//!
//! Extracted from razerd, thermaltaked and HeadsetBatteryIndicator, which each
//! carried their own copy of this.
