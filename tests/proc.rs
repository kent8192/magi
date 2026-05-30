use magi::error::MagiError;
use magi::proc::{executable_basename_is, parse_pid_file_contents};

#[test]
fn parse_pid_file_contents_accepts_positive_integer() {
    assert_eq!(parse_pid_file_contents("12345\n").unwrap(), Some(12345));
    assert_eq!(parse_pid_file_contents("  42  ").unwrap(), Some(42));
}

#[test]
fn parse_pid_file_contents_treats_empty_as_none() {
    assert_eq!(parse_pid_file_contents("").unwrap(), None);
    assert_eq!(parse_pid_file_contents("   \n\t").unwrap(), None);
}

#[test]
fn parse_pid_file_contents_rejects_zero_and_negative() {
    let zero = parse_pid_file_contents("0\n").unwrap_err();
    assert!(matches!(zero, MagiError::InvalidConfig(message) if message.contains("pid")));

    // A negative value such as "-1" must never reach `kill`, where it would be
    // interpreted as a signal/target selector rather than a process id.
    let negative = parse_pid_file_contents("-1\n").unwrap_err();
    assert!(matches!(negative, MagiError::InvalidConfig(message) if message.contains("pid")));
}

#[test]
fn parse_pid_file_contents_rejects_non_numeric() {
    let error = parse_pid_file_contents("not-a-pid\n").unwrap_err();
    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("pid")));
}

#[test]
fn executable_basename_is_matches_on_basename() {
    assert!(executable_basename_is("ssh", "ssh"));
    assert!(executable_basename_is("/usr/bin/ssh", "ssh"));
    assert!(executable_basename_is(
        "/opt/homebrew/bin/redis-server",
        "redis-server"
    ));
}

#[test]
fn executable_basename_is_rejects_mismatch_and_prefix() {
    // Exact match only: a reused pid running `sshd` or another program must not
    // be mistaken for our `ssh` tunnel.
    assert!(!executable_basename_is("/usr/bin/sshd", "ssh"));
    assert!(!executable_basename_is("bash", "ssh"));
    assert!(!executable_basename_is(
        "redis-server-extra",
        "redis-server"
    ));
}
