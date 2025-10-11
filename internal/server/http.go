// Package server owns the tiny HTTP surface of the exporter.
// It exposes helpers to construct the mux that serves /metrics and /healthz.
package server

import (
	"net/http"

	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// NewMuxWithHealth returns an http.ServeMux with:
//   - /metrics bound to the Prometheus exposition endpoint
//   - /healthz returning 200 (healthy) or 503 (unhealthy) using the provided function
func NewMuxWithHealth(isHealthy func() (bool, string)) *http.ServeMux {
	mux := http.NewServeMux()
	mux.Handle("/metrics", promhttp.Handler())
	mux.HandleFunc("/healthz", func(respWriter http.ResponseWriter, _ *http.Request) {
		ok, reason := isHealthy()
		if ok {
			respWriter.WriteHeader(http.StatusOK)
			_, _ = respWriter.Write([]byte("ok\n"))

			return
		}

		respWriter.WriteHeader(http.StatusServiceUnavailable)

		if reason == "" {
			reason = "unhealthy"
		}

		_, _ = respWriter.Write([]byte(reason + "\n"))
	})

	return mux
}
