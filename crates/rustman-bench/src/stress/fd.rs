//! The pre-flight file-descriptor check (stress-testing "Client saturation
//! guard", design "Risks"): every connection the client opens is a file
//! descriptor, so a limit below `max_rps × 2` can make the client fail
//! before the target does. The check only warns; the run goes on.

/// The soft limit on open files of this process, where the platform has
/// one (Unix); `None` elsewhere or when it is unlimited.
pub fn fd_limit() -> Option<u64> {
    #[cfg(unix)]
    {
        rustix::process::getrlimit(rustix::process::Resource::Nofile).current
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The warning for an open-file `limit` below `max_rps × 2`, if any.
pub fn fd_warning(limit: Option<u64>, max_rps: u32) -> Option<String> {
    let limit = limit?;
    let needed = u64::from(max_rps).saturating_mul(2);
    (limit < needed).then(|| {
        format!(
            "the open-file limit is {limit}, below 2 × max_rps ({needed}): the client may run out of \
             connections before the target breaks; raise it (ulimit -n {needed}) or lower max_rps"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_low_limit_is_warned_about() {
        let warning = fd_warning(Some(256), 500).unwrap();
        assert_eq!(
            warning,
            "the open-file limit is 256, below 2 × max_rps (1000): the client may run out of \
             connections before the target breaks; raise it (ulimit -n 1000) or lower max_rps"
        );
        assert_eq!(fd_warning(Some(1000), 500), None);
        assert_eq!(fd_warning(Some(65_536), 500), None);
        assert_eq!(fd_warning(None, 500), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_limit_of_this_process_is_read() {
        // Unlimited (None) is possible, but a limit, when set, is above 0.
        assert_ne!(fd_limit(), Some(0));
    }
}
