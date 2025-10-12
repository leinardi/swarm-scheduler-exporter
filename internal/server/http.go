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
