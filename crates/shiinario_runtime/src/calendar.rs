use anyhow::{Context, Result, ensure};

/// GetLocalTime field order, including Sunday = 0 and milliseconds.
pub fn local(time: bool) -> Result<[u32; 4]> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("host clock precedes Unix epoch")?;
    let seconds: libc::time_t = now.as_secs().try_into()?;
    let mut result = std::mem::MaybeUninit::<libc::tm>::uninit();
    // Both pointers are valid and localtime_r initializes the tm on success.
    let pointer = unsafe { libc::localtime_r(&seconds, result.as_mut_ptr()) };
    ensure!(!pointer.is_null(), "host local calendar conversion failed");
    let result = unsafe { result.assume_init() };
    Ok(if time {
        [
            result.tm_hour as u32,
            result.tm_min as u32,
            result.tm_sec as u32,
            now.subsec_millis(),
        ]
    } else {
        [
            (result.tm_year + 1900) as u32,
            (result.tm_mon + 1) as u32,
            result.tm_mday as u32,
            result.tm_wday as u32,
        ]
    })
}
