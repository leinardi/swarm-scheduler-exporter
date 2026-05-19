/*
 * MIT License
 *
 * Copyright (c) 2025 Roberto Leinardi
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

package server

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestHealthHandler_Healthy(t *testing.T) {
	handler := healthHandler(func() (bool, string) { return true, "" })
	req := httptest.NewRequestWithContext(
		context.Background(),
		http.MethodGet,
		"/healthz",
		http.NoBody,
	)
	rec := httptest.NewRecorder()
	handler(rec, req)

	res := rec.Result()
	defer res.Body.Close()

	if res.StatusCode != http.StatusOK {
		t.Errorf("status = %d, want 200", res.StatusCode)
	}

	body, _ := io.ReadAll(res.Body)
	if string(body) != okBody {
		t.Errorf("body = %q, want %q", body, okBody)
	}

	if ct := res.Header.Get("Content-Type"); ct != "text/plain; charset=utf-8" {
		t.Errorf("Content-Type = %q", ct)
	}
}

func TestHealthHandler_Unhealthy_WithReason(t *testing.T) {
	handler := healthHandler(func() (bool, string) { return false, "last poll too old" })
	req := httptest.NewRequestWithContext(
		context.Background(),
		http.MethodGet,
		"/healthz",
		http.NoBody,
	)
	rec := httptest.NewRecorder()
	handler(rec, req)

	res := rec.Result()
	defer res.Body.Close()

	if res.StatusCode != http.StatusServiceUnavailable {
		t.Errorf("status = %d, want 503", res.StatusCode)
	}

	body, _ := io.ReadAll(res.Body)
	if string(body) != "last poll too old\n" {
		t.Errorf("body = %q", body)
	}
}

func TestHealthHandler_Unhealthy_EmptyReason(t *testing.T) {
	handler := healthHandler(func() (bool, string) { return false, "" })
	req := httptest.NewRequestWithContext(
		context.Background(),
		http.MethodGet,
		"/healthz",
		http.NoBody,
	)
	rec := httptest.NewRecorder()
	handler(rec, req)

	res := rec.Result()
	defer res.Body.Close()

	if res.StatusCode != http.StatusServiceUnavailable {
		t.Errorf("status = %d, want 503", res.StatusCode)
	}

	body, _ := io.ReadAll(res.Body)
	if string(body) != "unhealthy\n" {
		t.Errorf("body = %q, want 'unhealthy\\n'", body)
	}
}
