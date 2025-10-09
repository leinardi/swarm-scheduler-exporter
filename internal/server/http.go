// Package server owns the tiny HTTP surface of the exporter.
// It exposes a helper to construct the mux that serves /metrics.
package server

import (
	"net/http"

	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// NewMux returns an http.ServeMux with a single /metrics handler bound
// to the Prometheus HTTP exposition endpoint.
func NewMux() *http.ServeMux {
	mux := http.NewServeMux()
	mux.Handle("/metrics", promhttp.Handler())

	return mux
}
