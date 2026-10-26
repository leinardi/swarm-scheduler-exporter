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

use super::*;

#[test]
fn filters_marshal_as_go_sorted_and_escaped() {
    let filters = Filters::new().add("type", &["service", "node"]);
    assert_eq!(
        filters.to_json(),
        r#"{"type":{"node":true,"service":true}}"#
    );
    let filters = Filters::new()
        .add("type", &["service"])
        .add("label", &["a<b&c>\"d\\\n\u{1}\u{2028}é"])
        .add("type", &["node"]);
    assert_eq!(
        filters.to_json(),
        r#"{"label":{"a\u003cb\u0026c\u003e\"d\\\n\u0001\u2028é":true},"type":{"node":true,"service":true}}"#
    );
    assert!(Filters::new().is_empty());
    assert_eq!(Filters::new().to_json(), "{}");
}

#[test]
fn queries_encode_as_url_values() {
    let filters = Filters::new().add("type", &["service", "node"]);
    let mut pairs = [
        (
            "since",
            since_parameter("2026-09-20T10:00:00.5Z".parse().unwrap()),
        ),
        ("filters", filters.to_json()),
    ];
    assert_eq!(
        encode_query(&mut pairs),
        "filters=%7B%22type%22%3A%7B%22node%22%3Atrue%2C%22service%22%3Atrue%7D%7D\
         &since=1789898400.500000000"
    );
    assert_eq!(encode_query(&mut [("all", "1".to_owned())]), "all=1");
    assert_eq!(
        encode_query(&mut [("k y", "a b+c/é~".to_owned())]),
        "k+y=a+b%2Bc%2F%C3%A9~"
    );
}

#[test]
fn since_is_seconds_and_nine_digits_of_nanoseconds() {
    let since = |text: &str| since_parameter(text.parse().unwrap());
    assert_eq!(since("2026-09-20T10:00:00Z"), "1789898400.000000000");
    assert_eq!(
        since("2026-09-20T10:00:00.000000001Z"),
        "1789898400.000000001"
    );
    // The reconnect case of the engine-wire test: 500ms before an event at …300.2.
    assert_eq!(
        since_parameter(jiff::Timestamp::from_nanosecond(1_789_902_299_700_000_000).unwrap()),
        "1789902299.700000000"
    );
    // Go's Unix() rounds down, and Nanosecond() is never negative.
    assert_eq!(since("1969-12-31T23:59:59.25Z"), "-1.250000000");
}

#[test]
fn an_empty_id_is_refused_before_any_request() {
    let error = trim_id("service", " \t").unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid service name or ID: value is empty"
    );
    assert_eq!(error.kind(), crate::docker::ErrorKind::InvalidParameter);
    assert_eq!(trim_id("container", " c-1 ").unwrap(), "c-1");
}

#[tokio::test(start_paused = true)]
async fn the_timeouts_end_a_call_with_deadline_exceeded() {
    assert_eq!(DOCKER_REQUEST_TIMEOUT, Duration::from_secs(15));
    assert_eq!(CONTAINERS_LIST_TIMEOUT, Duration::from_secs(5));
    assert_eq!(CONTAINERS_INSPECT_TIMEOUT, Duration::from_secs(10));

    let start = tokio::time::Instant::now();
    let error = with_docker_timeout(std::future::pending::<Result<(), Error>>())
        .await
        .unwrap_err();
    assert_eq!(start.elapsed(), DOCKER_REQUEST_TIMEOUT);
    assert_eq!(error.kind(), crate::docker::ErrorKind::DeadlineExceeded);
    assert_eq!(error.to_string(), "context deadline exceeded");

    assert_eq!(
        with_docker_timeout(async { Ok::<_, Error>(7) })
            .await
            .unwrap(),
        7
    );
}

/// The container inspects of one poll share one deadline: the second call gets what the first
/// left.
#[tokio::test(start_paused = true)]
async fn a_shared_deadline_bounds_calls_together() {
    let start = tokio::time::Instant::now();
    let deadline = start + CONTAINERS_INSPECT_TIMEOUT;
    let slow = async {
        tokio::time::sleep(Duration::from_secs(7)).await;
        Ok::<_, Error>(())
    };
    with_deadline(deadline, slow).await.unwrap();
    let error = with_deadline(deadline, async {
        tokio::time::sleep(Duration::from_secs(7)).await;
        Ok::<_, Error>(())
    })
    .await
    .unwrap_err();
    assert_eq!(error.kind(), crate::docker::ErrorKind::DeadlineExceeded);
    assert_eq!(start.elapsed(), CONTAINERS_INSPECT_TIMEOUT);
}
