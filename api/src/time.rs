use axerrno::{LinuxError, LinuxResult};
use axhal::time::TimeValue;
use linux_raw_sys::general::{
    __kernel_old_timespec, __kernel_old_timeval, __kernel_sock_timeval, __kernel_timespec,
    timespec, timeval,
};

/// A helper trait for converting from and to `TimeValue`.
pub trait TimeValueLike {
    /// Converts from `TimeValue`.
    fn from_time_value(tv: TimeValue) -> Self;

    /// Tries to convert into `TimeValue`.
    fn try_into_time_value(self) -> LinuxResult<TimeValue>;
}

impl TimeValueLike for TimeValue {
    fn from_time_value(tv: TimeValue) -> Self {
        tv
    }

    fn try_into_time_value(self) -> LinuxResult<TimeValue> {
        Ok(self)
    }
}

impl TimeValueLike for timespec {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_nsec: tv.subsec_nanos() as _,
        }
    }

    fn try_into_time_value(self) -> LinuxResult<TimeValue> {
        if self.tv_nsec < 0 || self.tv_nsec > 999_999_999 || self.tv_sec < 0 {
            return Err(LinuxError::EINVAL);
        }
        Ok(TimeValue::new(self.tv_sec as u64, self.tv_nsec as u32))
    }
}

impl TimeValueLike for __kernel_timespec {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_nsec: tv.subsec_nanos() as _,
        }
    }

    fn try_into_time_value(self) -> LinuxResult<TimeValue> {
        if self.tv_nsec < 0 || self.tv_nsec > 999_999_999 || self.tv_sec < 0 {
            return Err(LinuxError::EINVAL);
        }
        Ok(TimeValue::new(self.tv_sec as u64, self.tv_nsec as u32))
    }
}

impl TimeValueLike for __kernel_old_timespec {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_nsec: tv.subsec_nanos() as _,
        }
    }

    fn try_into_time_value(self) -> LinuxResult<TimeValue> {
        if self.tv_nsec < 0 || self.tv_nsec > 999_999_999 || self.tv_sec < 0 {
            return Err(LinuxError::EINVAL);
        }
        Ok(TimeValue::new(self.tv_sec as u64, self.tv_nsec as u32))
    }
}

impl TimeValueLike for timeval {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_usec: tv.subsec_micros() as _,
        }
    }

    fn try_into_time_value(self) -> LinuxResult<TimeValue> {
        if self.tv_usec < 0 || self.tv_usec > 999_999 || self.tv_sec < 0 {
            return Err(LinuxError::EINVAL);
        }
        Ok(TimeValue::new(
            self.tv_sec as u64,
            self.tv_usec as u32 * 1000,
        ))
    }
}

impl TimeValueLike for __kernel_old_timeval {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_usec: tv.subsec_micros() as _,
        }
    }

    fn try_into_time_value(self) -> LinuxResult<TimeValue> {
        if self.tv_usec < 0 || self.tv_usec > 999_999 || self.tv_sec < 0 {
            return Err(LinuxError::EINVAL);
        }
        Ok(TimeValue::new(
            self.tv_sec as u64,
            self.tv_usec as u32 * 1000,
        ))
    }
}

impl TimeValueLike for __kernel_sock_timeval {
    fn from_time_value(tv: TimeValue) -> Self {
        Self {
            tv_sec: tv.as_secs() as _,
            tv_usec: tv.subsec_micros() as _,
        }
    }

    fn try_into_time_value(self) -> LinuxResult<TimeValue> {
        if self.tv_usec < 0 || self.tv_usec > 999_999 || self.tv_sec < 0 {
            return Err(LinuxError::EINVAL);
        }
        Ok(TimeValue::new(
            self.tv_sec as u64,
            self.tv_usec as u32 * 1000,
        ))
    }
}
