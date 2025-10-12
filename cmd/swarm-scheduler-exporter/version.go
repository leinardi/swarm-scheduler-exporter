package main

// This file defines version variables for the binary. They are typically
// overridden at build time with -ldflags, e.g.:
//
//	go build -ldflags "-X main.version=1.2.3 -X main.commit=$(git rev-parse --short HEAD) -X main.date=$(date -u +%Y-%m-%d)"

// These values are logged once at startup.
var (
	version = "dev"
	commit  = "none"
	date    = "unknown"
)
