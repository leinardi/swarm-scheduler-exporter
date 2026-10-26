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

fn started(chunks: &[&[u8]]) -> u64 {
    let mut framer = Framer::new();
    for chunk in chunks {
        framer.feed(chunk);
    }
    framer.started()
}

#[test]
fn nothing_read_starts_nothing() {
    assert_eq!(started(&[]), 0);
    assert_eq!(started(&[b""]), 0);
}

#[test]
fn a_request_and_the_start_of_the_next_in_one_read() {
    assert_eq!(started(&[b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"]), 1);
    assert_eq!(started(&[b"GET / HTTP/1.1\r\nHost: x\r\n\r\nG"]), 2);
    assert_eq!(
        started(&[b"GET / HTTP/1.1\r\nHost: x\r\n\r\nGET / HTTP/1.1\r\n\r\nGET"]),
        3
    );
}

#[test]
fn the_head_may_end_in_bare_newlines_and_arrive_byte_by_byte() {
    assert_eq!(started(&[b"GET / HTTP/1.1\nHost: x\n\nG"]), 2);
    let request = b"GET / HTTP/1.1\r\nHost: x\r\n\r\nG";
    let bytes: Vec<&[u8]> = request.chunks(1).collect();
    assert_eq!(started(&bytes), 2);
}

#[test]
fn a_content_length_body_belongs_to_its_request() {
    let head = b"POST / HTTP/1.1\r\nHost: x\r\ncontent-LENGTH:  4 \r\n\r\n";
    assert_eq!(started(&[head, b"abcd"]), 1);
    assert_eq!(started(&[head, b"ab", b"cd"]), 1);
    assert_eq!(started(&[head, b"abcdG"]), 2);
    // A body of newlines is still body, not an empty line ending anything.
    assert_eq!(started(&[head, b"\r\n\r\n"]), 1);
}

#[test]
fn a_chunked_body_belongs_to_its_request() {
    let head = b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: gzip, chunked\r\n\r\n";
    let body = b"4;ext=1\r\nab\r\n\r\n0\r\nTrailer: y\r\n\r\n";
    assert_eq!(started(&[head, body]), 1);
    assert_eq!(started(&[head, body, b"G"]), 2);
    let bytes: Vec<&[u8]> = body.chunks(1).collect();
    let mut chunks = vec![&head[..]];
    chunks.extend(bytes);
    chunks.push(b"G");
    assert_eq!(started(&chunks), 2);
    // chunked wins over a Content-Length, and a coding other than chunked last is not chunked.
    assert_eq!(
        started(&[b"POST / HTTP/1.1\r\nContent-Length: 99\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\nG"]),
        2
    );
    assert_eq!(
        started(&[b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked, gzip\r\n\r\nG"]),
        2
    );
}

#[test]
fn an_unparsable_content_length_or_chunk_size_reads_as_no_body() {
    assert_eq!(
        started(&[b"POST / HTTP/1.1\r\nContent-Length: x\r\n\r\nG"]),
        2
    );
    assert_eq!(
        started(&[b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nG"]),
        2
    );
}

#[test]
fn an_overlong_header_line_is_not_buffered() {
    let mut framer = Framer::new();
    framer.feed(b"GET / HTTP/1.1\r\nX: ");
    framer.feed(&vec![b'a'; 1 << 20]);
    let State::Head { line, .. } = &framer.state else {
        panic!("not in the head: {:?}", framer.state);
    };
    assert_eq!(line.len(), MAX_LINE);
    framer.feed(b"\r\n\r\nG");
    assert_eq!(framer.started(), 2);
}
