//! The environment handed to the daemon and to Chrome: an allowlist, never the Redis secret.
use ks_browser::child_env;
use std::ffi::OsString;

fn vars(names: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    names.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect()
}

#[test]
fn only_allowlisted_names_pass_and_no_redis_secret() {
    let got: Vec<String> = child_env(vars(&[
        ("HOME", "/h"), ("PATH", "/bin"), ("TMPDIR", "/t"), ("LANG", "en_US.UTF-8"),
        ("KS_HOME", "/k"), ("KS_NOTIFY", "0"), ("KS_CHROME", "/c"), ("KS_QUOTA_HOURLY", "5"),
        ("KS_REDIS_PASSWORD", "secret"), ("KS_REDIS_URL", "redis://:secret@x"), ("KS_TEST_REDIS_URL", "redis://:secret@x"),
        ("AWS_SECRET_ACCESS_KEY", "x"), ("SSH_AUTH_SOCK", "/s"), ("LC_ALL", "C"), ("ks_home", "/lower"), ("KS", "x"),
    ]))
    .into_iter()
    .map(|(k, _)| k.into_string().unwrap())
    .collect();
    assert_eq!(got, ["HOME", "PATH", "TMPDIR", "LANG", "KS_HOME", "KS_NOTIFY", "KS_CHROME", "KS_QUOTA_HOURLY"]);
}

#[test]
fn values_are_passed_unchanged() {
    assert_eq!(child_env(vars(&[("HOME", "/Users/x y")])), vars(&[("HOME", "/Users/x y")]));
}

#[test]
fn linux_desktop_session_variables_pass_so_chrome_can_open_windows_and_reach_the_secret_store() {
    let session = ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "XDG_CURRENT_DESKTOP"];
    let mut input: Vec<(&str, &str)> = session.iter().map(|k| (*k, "v")).collect();
    // close relatives that are not needed stay out, and so does anything naming Redis
    input.extend([("XDG_CONFIG_HOME", "/c"), ("XDG_SESSION_TYPE", "x11"), ("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/s"), ("DISPLAY_REDIS", "x"), ("KS_REDIS_URL", "redis://:secret@x")]);
    let got: Vec<String> = child_env(vars(&input)).into_iter().map(|(k, _)| k.into_string().unwrap()).collect();
    assert_eq!(got, session);
}
