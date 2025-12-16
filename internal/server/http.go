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

// Package server owns the tiny HTTP surface of the exporter.
// It exposes helpers to construct the mux that serves /metrics and /healthz.
package server

import (
	"io"
	"net/http"

	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// HealthFunc returns whether the exporter is healthy and, if not, a short reason.
type HealthFunc func() (bool, string)

const (
	metricsPath = "/metrics"
	healthzPath = "/healthz"

	okBody       = "ok\n"
	defaultCause = "unhealthy\n"
)

// NewMuxWithHealth returns an http.ServeMux with:
//   - /metrics bound to the Prometheus exposition endpoint
//   - /healthz returning 200 (healthy) or 503 (unhealthy) using the provided function
func NewMuxWithHealth(isHealthy HealthFunc) *http.ServeMux {
	mux := http.NewServeMux()
	mux.Handle(metricsPath, promhttp.Handler())
	mux.HandleFunc(healthzPath, healthHandler(isHealthy))

	return mux
}

func healthHandler(isHealthy HealthFunc) http.HandlerFunc {
	return func(responseWriter http.ResponseWriter, _ *http.Request) {
		responseWriter.Header().Set("Content-Type", "text/plain; charset=utf-8")

		ok, reason := isHealthy()
		if ok {
			responseWriter.WriteHeader(http.StatusOK)
			_, _ = io.WriteString(responseWriter, okBody)

			return
		}

		if reason == "" {
			reason = defaultCause[:len(defaultCause)-1] // write without double newline below
		}

		responseWriter.WriteHeader(http.StatusServiceUnavailable)
		_, _ = io.WriteString(responseWriter, reason+"\n")
	}
}
