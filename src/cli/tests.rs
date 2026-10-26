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

use super::*;

/// The program name, then `arguments`.
fn parse_args(arguments: &[&str]) -> Result<Args, Exit> {
    parse(std::iter::once("swarm-scheduler-exporter").chain(arguments.iter().copied()))
}

/// The parsed flags, and what the run does next: the healthcheck, exit 1 on the poll-delay check,
/// or run.
fn summary(args: &Args) -> String {
    let next = if args.healthcheck {
        "healthcheck"
    } else if check_poll_delay(args.poll_delay).is_err() {
        "exit 1"
    } else {
        "run"
    };
    format!(
        "listen={} poll={} containers={} swarm={} format={:?} level={} time={} labels={:?} then {next}",
        args.listen_addr,
        duration::format(args.poll_delay),
        args.containers,
        args.containers_include_swarm,
        args.log_format,
        args.log_level,
        args.log_time,
        args.labels,
    )
}

/// The summary of the parsed flags, or the exit code and the first line printed.
type Outcome = Result<&'static str, (i32, &'static str)>;

/// Every S5 case in its `--` spelling, with its [`Outcome`].
const S5_CASES: &[(&[&str], Outcome)] = &[
    (
        &["-listen-addr=:9000"],
        Err((
            2,
            "error: single-dash long flags are not supported: '-listen-addr'; use '--listen-addr'",
        )),
    ),
    (
        &["-poll-delay", "5s"],
        Err((
            2,
            "error: single-dash long flags are not supported: '-poll-delay'; use '--poll-delay'",
        )),
    ),
    (
        &["-containers"],
        Err((
            2,
            "error: single-dash long flags are not supported: '-containers'; use '--containers'",
        )),
    ),
    (
        &["-help"],
        Err((
            2,
            "error: single-dash long flags are not supported: '-help'; use '--help'",
        )),
    ),
    (
        &["-h"],
        Err((0, "Usage: swarm-scheduler-exporter [OPTIONS]")),
    ),
    (
        &["--help"],
        Err((0, "Usage: swarm-scheduler-exporter [OPTIONS]")),
    ),
    (
        &["--listen-addr=:9000"],
        Ok(
            "listen=:9000 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--listen-addr", ":9000"],
        Ok(
            "listen=:9000 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--listen-addr", "-containers"],
        Ok(
            "listen=-containers poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--listen-addr", "--containers"],
        Ok(
            "listen=--containers poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--listen-addr", "-x"],
        Ok(
            "listen=-x poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=true swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=false"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers", "false"],
        Err((2, "error: unexpected argument 'false' found")),
    ),
    (
        &["--containers="],
        Err((
            2,
            "error: invalid value '' for '--containers[=<BOOL>]': parse error",
        )),
    ),
    (
        &["--log-level", "debug", "--log-level", "error"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=ERROR time=false labels=[] then run",
        ),
    ),
    (
        &["--log-level", "DEBUG", "--log-format", "JSON"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Json level=DEBUG time=false labels=[] then run",
        ),
    ),
    (
        &["--log-level", "Warning"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=WARN time=false labels=[] then run",
        ),
    ),
    (
        &["--label", ""],
        Err((
            2,
            "error: invalid value '' for '--label <value>': empty flag value",
        )),
    ),
    (
        &["--label="],
        Err((
            2,
            "error: invalid value '' for '--label <value>': empty flag value",
        )),
    ),
    (
        &["--label", "team", "--label", "tier"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[\"team\", \"tier\"] then run",
        ),
    ),
    (
        &["--label=team", "--label", "tier"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[\"team\", \"tier\"] then run",
        ),
    ),
    (
        &["--label", "-x"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[\"-x\"] then run",
        ),
    ),
    (
        &["--containers", "trailing"],
        Err((2, "error: unexpected argument 'trailing' found")),
    ),
    (
        &["--poll-delay", "5s", "trailing", "extra"],
        Err((2, "error: unexpected argument 'trailing' found")),
    ),
    (
        &["--", "--containers"],
        Err((2, "error: unexpected argument '--containers' found")),
    ),
    (
        &["--help=false"],
        Err((
            2,
            "error: unexpected value 'false' for '--help' found; no more were expected",
        )),
    ),
    (
        &["--unknown"],
        Err((2, "error: unexpected argument '--unknown' found")),
    ),
    (
        &["--unknown=1"],
        Err((2, "error: unexpected argument '--unknown' found")),
    ),
    (
        &["---containers"],
        Err((2, "error: unexpected argument '---containers' found")),
    ),
    (&["-c"], Err((2, "error: unexpected argument '-c' found"))),
    (&["-x"], Err((2, "error: unexpected argument '-x' found"))),
    (
        &["--cont"],
        Err((2, "error: unexpected argument '--cont' found")),
    ),
    (
        &["--poll-delay", "1h2m"],
        Ok(
            "listen=0.0.0.0:8888 poll=1h2m0s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--poll-delay", "1.5s"],
        Ok(
            "listen=0.0.0.0:8888 poll=1.5s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--poll-delay", "0"],
        Ok(
            "listen=0.0.0.0:8888 poll=0s containers=false swarm=false format=Text level=INFO time=false labels=[] then exit 1",
        ),
    ),
    (
        &["--poll-delay", "-5s"],
        Ok(
            "listen=0.0.0.0:8888 poll=-5s containers=false swarm=false format=Text level=INFO time=false labels=[] then exit 1",
        ),
    ),
    (
        &["--poll-delay=-5s"],
        Ok(
            "listen=0.0.0.0:8888 poll=-5s containers=false swarm=false format=Text level=INFO time=false labels=[] then exit 1",
        ),
    ),
    (
        &["--poll-delay", "10"],
        Err((
            2,
            "error: invalid value '10' for '--poll-delay <duration>': time: missing unit in duration \"10\"",
        )),
    ),
    (
        &["--poll-delay", "500ms"],
        Ok(
            "listen=0.0.0.0:8888 poll=500ms containers=false swarm=false format=Text level=INFO time=false labels=[] then exit 1",
        ),
    ),
    (
        &["--containers=1"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=true swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=t"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=true swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=T"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=true swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=TRUE"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=true swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=true"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=true swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=True"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=true swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=0"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=f"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=F"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=FALSE"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=false"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=False"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--containers=yes"],
        Err((
            2,
            "error: invalid value 'yes' for '--containers[=<BOOL>]': parse error",
        )),
    ),
    (
        &["--containers=tRuE"],
        Err((
            2,
            "error: invalid value 'tRuE' for '--containers[=<BOOL>]': parse error",
        )),
    ),
    (
        &["--containers=true", "--containers=false"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[] then run",
        ),
    ),
    (
        &["--log-level=bogus"],
        Err((
            2,
            "error: invalid value 'bogus' for '--log-level <string>': invalid log level \"bogus\" (want one of debug, info, warn, warning, error, fatal, panic)",
        )),
    ),
    (
        &["--log-format=xml"],
        Err((
            2,
            "error: invalid value 'xml' for '--log-format <string>': invalid log format \"xml\" (want one of json, text, plain)",
        )),
    ),
    (
        &["--log-level"],
        Err((
            2,
            "error: a value is required for '--log-level <string>' but none was supplied",
        )),
    ),
    (
        &["--listen-addr"],
        Err((
            2,
            "error: a value is required for '--listen-addr <string>' but none was supplied",
        )),
    ),
    (
        &["--healthcheck", "--poll-delay", "0"],
        Ok(
            "listen=0.0.0.0:8888 poll=0s containers=false swarm=false format=Text level=INFO time=false labels=[] then healthcheck",
        ),
    ),
    (
        &["--poll-delay", "0", "--healthcheck"],
        Ok(
            "listen=0.0.0.0:8888 poll=0s containers=false swarm=false format=Text level=INFO time=false labels=[] then healthcheck",
        ),
    ),
    (
        &["--poll-delay", "-h"],
        Err((
            2,
            "error: invalid value '-h' for '--poll-delay <duration>': time: invalid duration \"-h\"",
        )),
    ),
    (
        &["--label", "-label"],
        Ok(
            "listen=0.0.0.0:8888 poll=10s containers=false swarm=false format=Text level=INFO time=false labels=[\"-label\"] then run",
        ),
    ),
];

#[test]
fn s5_cases() {
    let mut mismatches = Vec::new();
    for (arguments, want) in S5_CASES {
        let got = parse_args(arguments)
            .map(|args| summary(&args))
            .map_err(|exit| {
                (
                    exit.code,
                    exit.message.lines().next().unwrap_or_default().to_owned(),
                )
            });
        let want = want
            .map(str::to_owned)
            .map_err(|(code, line)| (code, line.to_owned()));
        if got != want {
            mismatches.push(format!("{arguments:?}\n  got  {got:?}\n  want {want:?}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
/// `-h` and `--help` print the Go help texts, in clap's layout, to stdout and exit 0 (Q3).
#[test]
fn help_has_the_go_texts_and_exits_zero() {
    const HELP: &str = "\
Usage: swarm-scheduler-exporter [OPTIONS]

Options:
      --listen-addr <string>
          IP address and port to bind [default: 0.0.0.0:8888]
      --poll-delay <duration>
          How often to poll tasks (Go duration, e.g. 10s, 1m). Minimum 1s. [default: 10s]
      --containers[=<BOOL>]
          Expose container state metrics (opt-in).
      --containers-include-swarm[=<BOOL>]
          Include containers belonging to Swarm tasks.
      --log-format <string>
          Either json, text or plain [default: text]
      --log-level <string>
          Either debug, info, warn, error, fatal, panic [default: info]
      --log-time[=<BOOL>]
          Include timestamp in logs
      --healthcheck[=<BOOL>]
          Probe this exporter's /healthz on --listen-addr and exit 0 if healthy, 1 otherwise
      --label <value>
          Name of custom service labels to add to metrics
  -h, --help
          Display help message
";
    for flag in ["-h", "--help"] {
        let exit = parse_args(&[flag]).unwrap_err();
        assert_eq!(
            exit,
            Exit {
                code: 0,
                stdout: true,
                message: HELP.to_owned()
            },
            "{flag}"
        );
    }
}

/// A parse error goes to stderr with exit 2; the pre-scan's too.
#[test]
fn errors_go_to_stderr_with_exit_two() {
    for arguments in [
        &["--unknown"][..],
        &["-unknown"],
        &["-hx"],
        &["--containers", "false"],
    ] {
        let exit = parse_args(arguments).unwrap_err();
        assert_eq!((exit.code, exit.stdout), (2, false), "{arguments:?}");
        assert!(
            exit.message.ends_with('\n'),
            "{arguments:?}: {:?}",
            exit.message
        );
    }
}

/// The pre-scan refuses `-hx` (clap alone prints the help for the `-h` cluster) and stops at
/// `--`, leaving what follows to clap, which refuses it as a positional.
#[test]
fn single_dash_scan_covers_clusters_and_stops_at_double_dash() {
    assert_eq!(
        parse_args(&["-hx"]).unwrap_err().message,
        "error: flag provided but not defined: '-hx'\n"
    );
    assert_eq!(
        parse_args(&["-unknown=1"]).unwrap_err().message,
        "error: flag provided but not defined: '-unknown'\n"
    );
    // The value counts: clap would run -h=false as -h and print the help.
    for (argument, name) in [("-h=false", "-h"), ("-h=1", "-h"), ("-x=1", "-x")] {
        assert_eq!(
            parse_args(&[argument]).unwrap_err(),
            Exit {
                code: 2,
                stdout: false,
                message: format!("error: flag provided but not defined: '{name}'\n"),
            },
            "{argument}"
        );
    }
    // -h alone is the help, and a lone dash is left to clap, which refuses it as a positional.
    assert_eq!(parse_args(&["-h"]).unwrap_err().code, 0);
    assert_eq!(parse_args(&["-"]).unwrap_err().code, 2);
    assert_eq!(
        parse_args(&["--label", "-poll-delay"]).unwrap().labels,
        ["-poll-delay"]
    );
    // The value of --listen-addr=x is in the same argument, so the next one is scanned.
    assert_eq!(
        parse_args(&["--listen-addr=:1", "-containers"])
            .unwrap_err()
            .message,
        "error: single-dash long flags are not supported: '-containers'; use '--containers'\n"
    );
    let after = parse_args(&["--", "-containers"]).unwrap_err();
    assert_eq!(
        after.message.lines().next(),
        Some("error: unexpected argument '-containers' found")
    );
}

/// The defaults are Go's.
#[test]
fn defaults_are_go_defaults() {
    let args = parse_args(&[]).unwrap();
    assert_eq!(
        args,
        Args {
            listen_addr: "0.0.0.0:8888".to_owned(),
            poll_delay: DEFAULT_POLL_DELAY,
            containers: false,
            containers_include_swarm: false,
            log_format: Format::Text,
            log_level: Level::INFO,
            log_time: false,
            healthcheck: false,
            labels: Vec::new(),
            help: None,
        }
    );
}

/// The poll-delay message names Go's minimum as Go printed it.
#[test]
fn poll_delay_check_wording() {
    assert_eq!(check_poll_delay(MIN_POLL_DELAY), Ok(()));
    assert_eq!(
        check_poll_delay(MIN_POLL_DELAY - 1)
            .unwrap_err()
            .to_string(),
        "poll-delay must be >= 1s"
    );
}

/// Go: `TestStringSlice_Set_EmptyErrors`
#[test]
fn empty_label_is_refused() {
    assert_eq!(parse_label(""), Err(EmptyFlagValue));
    for arguments in [&["--label", ""][..], &["--label="]] {
        let exit = parse_args(arguments).unwrap_err();
        assert_eq!(exit.code, 2);
        assert_eq!(
            exit.message.lines().next(),
            Some("error: invalid value '' for '--label <value>': empty flag value"),
        );
    }
}

/// Go: `TestStringSlice_Set_Appends`
#[test]
fn labels_append_in_order() {
    assert_eq!(parse_label("a"), Ok("a".to_owned()));
    assert_eq!(
        parse_args(&["--label", "a", "--label=b"]).unwrap().labels,
        ["a", "b"]
    );
}

fn raw(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|&key| key.to_owned()).collect()
}

/// Go: `TestValidateAndSetCustomLabels_Valid`
#[test]
fn custom_labels_valid() {
    assert_eq!(
        validate_custom_labels(&raw(&["team", "com.example.tier"])),
        Ok(vec![
            CustomLabel {
                raw: "team".to_owned(),
                sanitized: "team".to_owned(),
            },
            CustomLabel {
                raw: "com.example.tier".to_owned(),
                sanitized: "com_example_tier".to_owned(),
            },
        ])
    );
}

/// Go: `TestValidateAndSetCustomLabels_TooMany`
#[test]
fn custom_labels_too_many() {
    let error =
        validate_custom_labels(&raw(&["a", "b", "c", "d", "e", "f", "g", "h", "i"])).unwrap_err();
    assert!(matches!(error, CustomLabelsError::Count(_)), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with("validate custom label count: "),
        "{error}"
    );
}

/// Go: `TestValidateAndSetCustomLabels_InvalidName_ReservedPrefix`
#[test]
fn custom_labels_reserved_prefix() {
    let error = validate_custom_labels(&raw(&["__reserved"])).unwrap_err();
    assert!(matches!(error, CustomLabelsError::Sanitize(_)), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with("sanitize custom label names: "),
        "{error}"
    );
}

/// Go: `TestValidateAndSetCustomLabels_ReservedName`
#[test]
fn custom_labels_reserved_name() {
    for (key, name) in [
        ("stack", "stack"),
        ("service", "service"),
        ("service_mode", "service_mode"),
        ("display.name", "display_name"),
        ("state", "state"),
    ] {
        let error = validate_custom_labels(&raw(&["team", key])).unwrap_err();
        assert_eq!(error, CustomLabelsError::Reserved(name.to_owned()), "{key}");
        assert_eq!(
            error.to_string(),
            format!("custom label collides with a reserved label name: \"{name}\"")
        );
    }
}

/// Go: `TestValidateAndSetCustomLabels_PostSanitizeCollision`
#[test]
fn custom_labels_post_sanitize_collision() {
    let error = validate_custom_labels(&raw(&["foo.bar", "foo-bar"])).unwrap_err();
    assert!(matches!(error, CustomLabelsError::Sanitize(_)), "{error:?}");
}

/// Go: `TestValidateAndSetCustomLabels_Empty`
#[test]
fn custom_labels_empty() {
    assert_eq!(validate_custom_labels(&[]), Ok(Vec::new()));
}
