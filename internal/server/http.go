package server

import (
	"net/http"

	"github.com/prometheus/client_golang/prometheus/promhttp"
)

func NewMux() *http.ServeMux {
	mux := http.NewServeMux()
	mux.Handle("/metrics", promhttp.Handler())

	return mux
}
