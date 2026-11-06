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

use docker_engine::with_bare_deadline;

use super::*;

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
    with_bare_deadline(deadline, slow).await.unwrap();
    let error = with_bare_deadline(deadline, async {
        tokio::time::sleep(Duration::from_secs(7)).await;
        Ok::<_, Error>(())
    })
    .await
    .unwrap_err();
    assert_eq!(error.kind(), crate::docker::ErrorKind::DeadlineExceeded);
    assert_eq!(start.elapsed(), CONTAINERS_INSPECT_TIMEOUT);
}
