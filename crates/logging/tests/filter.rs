// SPDX-License-Identifier: GPL-3.0-only
//! The filter changes at run time through `developer` and `log_filter`, and the log file is written.

use tracing::Level;

#[test]
fn developer_and_filter_change_what_is_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = logging::Config::new("test");
    cfg.stderr = false;
    cfg.dir = Some(dir.path().to_path_buf());
    let guard = logging::init(cfg);

    assert!(tracing::enabled!(target: "logging", Level::INFO));
    assert!(!tracing::enabled!(target: "logging", Level::DEBUG));
    logging::set_developer(1);
    assert!(tracing::enabled!(target: "logging", Level::DEBUG));
    assert!(!tracing::enabled!(target: "logging", Level::TRACE));
    logging::set_developer(2);
    assert!(tracing::enabled!(target: "logging", Level::TRACE));
    logging::set_developer(0);
    assert!(!tracing::enabled!(target: "logging", Level::DEBUG));

    assert!(logging::set_filter("logging=nonsense").is_err());
    assert!(
        !tracing::enabled!(target: "logging", Level::DEBUG),
        "a bad filter keeps the old one"
    );
    logging::set_filter("logging=error").unwrap();
    assert!(!tracing::enabled!(target: "logging", Level::INFO));

    logging::set_filter("").unwrap();
    tracing::warn!(target: "logging", thing = "x", "something unexpected");
    let text = std::fs::read_to_string(guard.path().expect("a log file")).unwrap();
    assert!(
        text.contains("something unexpected") && text.contains("thing"),
        "{text}"
    );
}
