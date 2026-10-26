/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use super::{
    DEFAULT_STALE_AFTER, ENV_STALE_AFTER, Found, Owner, SweepRules, parse_owner, round,
    stale_after_from_env,
};

fn timestamp(text: &str) -> Timestamp {
    text.parse()
        .unwrap_or_else(|error| panic!("timestamp {text}: {error}"))
}

/// The time as the harness writes it in the started label: RFC 3339, UTC, whole seconds.
fn rfc3339(at: Timestamp) -> String {
    at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Go: `TestParseOwner`.
#[test]
fn parse_owner_takes_the_pid_after_the_last_colon() {
    let owner = parse_owner("runner:fe80::1:4242").unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        (owner.host.as_str(), owner.pid),
        ("runner:fe80::1", 4242),
        "parse_owner kept {owner:?}; want the PID after the last colon"
    );

    let formatted = Owner {
        host: "ci".to_owned(),
        pid: 7,
        pid_namespace: "pid:[1]".to_owned(),
    }
    .to_string();
    assert_eq!(
        formatted, "ci:7",
        "an owner does not format as <hostname>:<pid>"
    );

    for bad in ["", "host", ":12", "host:", "host:x", "host:0", "host:-3"] {
        assert!(parse_owner(bad).is_err(), "parse_owner({bad:?}) succeeded");
    }
}

fn found(id: &str, owner: &str, started: &str, pid_namespace: &str) -> Found {
    Found {
        id: id.to_owned(),
        owner: owner.to_owned(),
        started: started.to_owned(),
        pid_namespace: pid_namespace.to_owned(),
    }
}

fn rules(now: Timestamp, pid_namespace: &str) -> SweepRules {
    SweepRules {
        now,
        stale_after: Duration::from_secs(3600),
        host: "here".to_owned(),
        pid_namespace: pid_namespace.to_owned(),
        self_id: "self".to_owned(),
        alive: Box::new(|pid| pid == 100),
    }
}

/// Go: `TestSweepRulesDecide`.
#[test]
fn sweep_rules_decide() {
    let now = timestamp("2026-10-11T12:00:00Z");
    let old = rfc3339(now - SignedDuration::from_hours(2));
    let recent = rfc3339(now - SignedDuration::from_mins(10));

    let verdict = rules(now, "").decide(found("a", "here:200", &old, ""));
    assert!(
        !verdict.remove,
        "a sweeper that cannot read its own PID namespace removed an environment"
    );

    let rules = rules(now, "pid:[1]");
    let cases = [
        (
            "the current environment",
            found("self", "here:200", &old, "pid:[1]"),
            false,
            "current",
        ),
        (
            "recent, owner gone",
            found("a", "here:200", &recent, "pid:[1]"),
            false,
            "started 10m0s ago, under 1h0m0s",
        ),
        (
            "stale, owner running",
            found("a", "here:100", &old, "pid:[1]"),
            false,
            "still running",
        ),
        (
            "stale, another host",
            found("a", "there:200", &old, ""),
            false,
            "another host",
        ),
        (
            "stale, no owner label",
            found("a", "", &old, ""),
            false,
            "owner label",
        ),
        (
            "no started label",
            found("a", "here:200", "", ""),
            false,
            "started label",
        ),
        (
            "stale, owner gone",
            found("a", "here:200", &old, "pid:[1]"),
            true,
            "gone",
        ),
        (
            "stale, owner gone in another PID namespace",
            found("a", "here:200", &old, "pid:[2]"),
            false,
            "PID namespace",
        ),
        (
            "stale, owner gone, no PID namespace label",
            found("a", "here:200", &old, ""),
            false,
            "PID namespace",
        ),
    ];
    let mut failures = Vec::new();
    for (name, env, remove, reason) in cases {
        let verdict = rules.decide(env);
        if verdict.remove != remove || !verdict.reason.contains(reason) {
            failures.push(format!(
                "{name}: remove={} ({}), want remove={remove} with {reason:?}",
                verdict.remove, verdict.reason
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Go: `TestStaleAfterFromEnv`. The variable comes through a lookup function rather than the
/// process environment, so the test runs in parallel with the others.
#[test]
fn stale_after_from_env_rejects_what_is_not_a_positive_duration() {
    let with =
        |value: &'static str| move |key: &str| (key == ENV_STALE_AFTER).then(|| value.to_owned());

    assert_eq!(
        stale_after_from_env(|_| None),
        Ok(DEFAULT_STALE_AFTER),
        "unset"
    );
    assert_eq!(
        stale_after_from_env(with("")),
        Ok(DEFAULT_STALE_AFTER),
        "empty"
    );
    assert_eq!(
        stale_after_from_env(with("90m")),
        Ok(Duration::from_mins(90)),
        "90m"
    );

    for bad in ["0s", "-1h", "soon", "10"] {
        let error = stale_after_from_env(with(bad))
            .err()
            .unwrap_or_else(|| panic!("{bad:?} was accepted; a typo must not shorten the window"));
        assert!(
            error
                .to_string()
                .contains(&format!("{ENV_STALE_AFTER}=\"{bad}\"")),
            "{error}"
        );
    }
}

/// Go's `Duration.Round`, which the age in a keep reason goes through.
#[test]
fn round_matches_go() {
    const MINUTE: i64 = 60_000_000_000;
    assert_eq!(round(89_000_000_000, MINUTE), MINUTE);
    assert_eq!(
        round(90_000_000_000, MINUTE),
        2 * MINUTE,
        "a tie rounds away from zero"
    );
    assert_eq!(round(-90_000_000_000, MINUTE), -2 * MINUTE);
    assert_eq!(round(-89_000_000_000, MINUTE), -MINUTE);
    assert_eq!(round(i64::MAX, MINUTE), 9_223_372_020_000_000_000);
    assert_eq!(round(i64::MAX, 10), i64::MAX, "saturates");
    assert_eq!(round(i64::MIN, 10), i64::MIN, "saturates");
    assert_eq!(round(7, 0), 7);
}
