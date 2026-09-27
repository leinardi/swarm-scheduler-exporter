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

// Package main wires and runs the exporter binary.
// It owns CLI flag parsing, logging setup, and the HTTP server with timeouts.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"slices"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/moby/moby/client"
	"github.com/moby/moby/client/pkg/versions"

	"github.com/leinardi/swarm-scheduler-exporter/internal/collector"
	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
	"github.com/leinardi/swarm-scheduler-exporter/internal/server"
)

const (
	// DefaultPollDelay is the default interval between polls.
	DefaultPollDelay = 10 * time.Second

	// Operability constants.
	minPollDelay        = 1 * time.Second
	httpShutdownTimeout = 10 * time.Second

	// healthcheckTimeout bounds a whole -healthcheck probe: connecting, the request and reading
	// the reason. It stays under the image HEALTHCHECK's --timeout.
	healthcheckTimeout = 3 * time.Second
	// healthcheckReasonLimit bounds how much of an unhealthy answer's body the probe reads.
	healthcheckReasonLimit = 4 << 10
)

// stringSlice implements flag.Value to support repeated -label flags
// (e.g., -label team -label tier). Each call to Set appends a value.
type stringSlice []string

// ErrEmptyFlagValue is returned when a repeated flag (like -label)
// is provided with an empty value. This is a sentinel error for tests
// and for clearer calling code.
var ErrEmptyFlagValue = errors.New("empty flag value")

// ErrReservedLabelName is returned when a custom label sanitizes to a label name the exporter
// already uses on its per-service metrics.
var ErrReservedLabelName = errors.New("custom label collides with a reserved label name")

// String returns the flag value in a human-friendly form.
func (values *stringSlice) String() string {
	return fmt.Sprint(*values)
}

// Set implements flag.Value for stringSlice by appending non-empty values.
func (values *stringSlice) Set(value string) error {
	if value == "" {
		return ErrEmptyFlagValue
	}

	*values = append(*values, value)

	return nil
}

var (
	// CLI flags.
	listenAddr = flag.String("listen-addr", "0.0.0.0:8888", "IP address and port to bind")
	pollDelay  = flag.Duration(
		"poll-delay",
		DefaultPollDelay,
		"How often to poll tasks (Go duration, e.g. 10s, 1m). Minimum 1s.",
	)
	enableContainers = flag.Bool(
		"containers",
		false,
		"Expose container state metrics (opt-in).",
	)
	containersIncludeSwarm = flag.Bool(
		"containers-include-swarm",
		false,
		"Include containers belonging to Swarm tasks.",
	)
	logFormat = flag.String("log-format", "text", "Either json, text or plain")
	// Quieter by default to reduce chatter in production.
	logLevel = flag.String("log-level", "info", "Either debug, info, warn, error, fatal, panic")
	logTime  = flag.Bool("log-time", false, "Include timestamp in logs")
	help     = flag.Bool("help", false, "Display help message")

	healthcheck = flag.Bool(
		"healthcheck",
		false,
		"Probe this exporter's /healthz on -listen-addr and exit 0 if healthy, 1 otherwise",
	)

	customLabels stringSlice
)

// usage prints flag usage to stdout. We avoid fmt.Print* linters by
// writing to an explicit writer and by setting the flag package's output.
func usage() {
	outWriter := os.Stdout
	flag.CommandLine.SetOutput(outWriter)

	// Avoid printing os.Args[0] to satisfy gosec's taint rule (G705).
	_, _ = fmt.Fprintln(outWriter, "Usage:")

	flag.PrintDefaults()
}

// main initializes logging, Prometheus collectors, the Docker client,
// starts the polling and events goroutines, and serves /metrics with timeouts.
func main() {
	os.Exit(run())
}

// run contains the full program logic and returns an exit code.
// Defers inside run() (e.g., cancel(), Close(), etc.) will execute.
func run() int {
	flag.Var(&customLabels, "label", "Name of custom service labels to add to metrics")
	flag.Parse()

	if *help {
		usage()

		return 0
	}

	// A probe of the running exporter, e.g. the image HEALTHCHECK: it touches no logger, no
	// metric and no Docker client.
	if *healthcheck {
		return runHealthcheck(*listenAddr, os.Stdout)
	}

	if *pollDelay < minPollDelay {
		_, _ = fmt.Fprintf(os.Stderr, "poll-delay must be >= %s\n", minPollDelay)

		return 1
	}

	// Configure slog logger according to flags.
	_ = logger.Configure(*logFormat, *logLevel, *logTime)
	loggerInstance := logger.L()

	// Log version info for diagnostics and to keep ldflags-injected vars "used".
	loggerInstance.Info("swarm-scheduler-exporter starting",
		"version", version,
		"commit", commit,
		"date", date,
	)

	// Validate + set custom labels
	validateErr := validateAndSetCustomLabels([]string(customLabels))
	if validateErr != nil {
		loggerInstance.Error("invalid custom labels", "err", validateErr)

		return 1
	}

	registerMetrics(*pollDelay, *enableContainers, *containersIncludeSwarm)

	// Root context canceled on SIGINT/SIGTERM
	rootContext, cancelRoot := signal.NotifyContext(
		context.Background(),
		syscall.SIGINT,
		syscall.SIGTERM,
	)
	defer cancelRoot()

	// Docker client is configured from environment variables (DOCKER_HOST, DOCKER_API_VERSION,
	// etc.). API version negotiation is lazy: it happens on the first request.
	dockerClient, newClientErr := newDockerClient()
	if newClientErr != nil {
		loggerInstance.Error("docker client init failed", "err", newClientErr)

		return 1
	}
	defer dockerClient.Close()

	versionErr := validateClientAPIVersion(dockerClient)
	if versionErr != nil {
		loggerInstance.Error("unsupported Docker API version", "err", versionErr)

		return 1
	}

	// WaitGroup to wait for goroutines (reconciler + event listener + poller).
	var workerGroup sync.WaitGroup

	startWorkers(rootContext, &workerGroup, dockerClient, *pollDelay)

	// HTTP server with sane timeouts + graceful shutdown.
	isHealthy := func() (bool, string) {
		return collector.HealthSnapshot(*pollDelay, time.Now())
	}
	httpMux := server.NewMuxWithHealth(isHealthy)

	return serveUntilDone(rootContext, cancelRoot, &workerGroup, *listenAddr, httpMux)
}

// --- helpers to reduce main() complexity ---

// runHealthcheck probes the /healthz of an exporter serving on listenAddr and returns the exit
// code: 0 when it answers exactly 200, 1 otherwise, after writing why to out. It exists because
// the image is distroless: there is no shell or curl a HEALTHCHECK could run instead.
func runHealthcheck(listenAddr string, out io.Writer) int {
	healthURL, urlErr := healthcheckURL(listenAddr)
	if urlErr != nil {
		_, _ = fmt.Fprintf(out, "healthcheck: %v\n", urlErr)

		return 1
	}

	probeContext, cancelProbe := context.WithTimeout(context.Background(), healthcheckTimeout)
	defer cancelProbe()

	request, requestErr := http.NewRequestWithContext(
		probeContext,
		http.MethodGet,
		healthURL,
		http.NoBody,
	)
	if requestErr != nil {
		_, _ = fmt.Fprintf(out, "healthcheck: %v\n", requestErr)

		return 1
	}

	response, doErr := newHealthcheckClient().Do(request)
	if doErr != nil {
		_, _ = fmt.Fprintf(out, "healthcheck: %v\n", doErr)

		return 1
	}

	defer func() { _ = response.Body.Close() }()

	if response.StatusCode == http.StatusOK {
		return 0
	}

	reason := healthcheckReason(response.Body)
	if reason == "" {
		_, _ = fmt.Fprintf(out, "healthcheck: %s: %s\n", healthURL, response.Status)
	} else {
		_, _ = fmt.Fprintf(out, "healthcheck: %s: %s: %s\n", healthURL, response.Status, reason)
	}

	return 1
}

// healthcheckURL returns the /healthz URL of an exporter serving on listenAddr. A wildcard host
// is not an address to connect to, so it becomes the loopback of the same family; any other host
// is kept, since the exporter listens there only.
func healthcheckURL(listenAddr string) (string, error) {
	host, port, splitErr := net.SplitHostPort(listenAddr)
	if splitErr != nil {
		return "", fmt.Errorf("parse -listen-addr: %w", splitErr)
	}

	switch host {
	case "", "0.0.0.0":
		host = "127.0.0.1"
	case "::":
		host = "::1"
	}

	healthURL := url.URL{
		Scheme: "http",
		Host:   net.JoinHostPort(host, port),
		Path:   server.HealthzPath,
	}

	return healthURL.String(), nil
}

// newHealthcheckClient returns the client of the -healthcheck probe. Its transport has no proxy:
// Go already skips HTTP_PROXY for loopback, but a non-loopback -listen-addr must not be probed
// through one either. A redirect is not followed: the 3xx is the final answer, and not a 200.
func newHealthcheckClient() *http.Client {
	return &http.Client{
		Transport: &http.Transport{
			Proxy:             nil,
			DisableKeepAlives: true,
		},
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
}

// healthcheckReason returns the first line of an unhealthy answer's body, trimmed, reading at most
// healthcheckReasonLimit bytes of it. What could be read before an error is still used.
func healthcheckReason(body io.Reader) string {
	limited, _ := io.ReadAll(io.LimitReader(body, healthcheckReasonLimit))
	firstLine, _, _ := strings.Cut(string(limited), "\n")

	return strings.TrimSpace(firstLine)
}

// serveUntilDone serves handler on address until rootContext ends or the server fails, then
// cancels rootContext so the workers return, and waits for them. It returns the exit code:
// 0 on a requested shutdown, 1 when the server failed on its own (e.g. the address is in use).
func serveUntilDone(
	rootContext context.Context,
	cancelRoot context.CancelFunc,
	workerGroup *sync.WaitGroup,
	address string,
	handler http.Handler,
) int {
	exitCode := 0

	runError := runHTTPServer(rootContext, address, handler)
	if runError != nil && !errors.Is(runError, http.ErrServerClosed) &&
		!errors.Is(runError, context.Canceled) {
		logger.L().Error("http server error", "err", runError)

		exitCode = 1
	}

	// Stop the workers: on a server failure rootContext is still live, and nothing else would
	// end them. A non-zero exit then lets the orchestrator restart the exporter.
	cancelRoot()
	workerGroup.Wait()

	return exitCode
}

// ErrUnsupportedAPIVersion is returned when DOCKER_API_VERSION pins a version outside the range
// the Docker client supports.
var ErrUnsupportedAPIVersion = errors.New("unsupported Docker API version")

// validateClientAPIVersion refuses an API version outside client.MinAPIVersion..client.MaxAPIVersion.
// Without DOCKER_API_VERSION the client starts at its maximum and negotiates down on the first
// request (refusing a daemon below the minimum there); with it, the pinned version is used as is
// and no negotiation happens, so a pin outside the range would otherwise only surface as request
// failures.
func validateClientAPIVersion(dockerClient *client.Client) error {
	apiVersion := dockerClient.ClientVersion()
	if versions.LessThan(apiVersion, client.MinAPIVersion) ||
		versions.GreaterThan(apiVersion, client.MaxAPIVersion) {
		return fmt.Errorf(
			"%w %s (DOCKER_API_VERSION): supported range is %s to %s",
			ErrUnsupportedAPIVersion,
			apiVersion,
			client.MinAPIVersion,
			client.MaxAPIVersion,
		)
	}

	return nil
}

func validateAndSetCustomLabels(rawKeys []string) error {
	countErr := labelutil.ValidateCustomLabelCount(len(rawKeys))
	if countErr != nil {
		return fmt.Errorf("validate custom label count: %w", countErr)
	}

	sanitized, sanitizeErr := labelutil.ValidateAndSanitizeLabelNames(rawKeys)
	if sanitizeErr != nil {
		return fmt.Errorf("sanitize custom label names: %w", sanitizeErr)
	}

	reserved := collector.ReservedLabelNames()
	for _, name := range sanitized {
		if slices.Contains(reserved, name) {
			return fmt.Errorf("%w: %q", ErrReservedLabelName, name)
		}
	}

	collector.SetCustomLabels(rawKeys, sanitized)

	return nil
}

// registerMetrics registers every metric family on the default registerer, including health and
// build info; the containers family only when enabled.
func registerMetrics(pollDelay time.Duration, enableContainers, containersIncludeSwarm bool) {
	collector.ConfigureDesiredReplicasGauge()
	collector.ConfigureReplicasStateGauge()
	collector.ConfigureHealthGauges(version, commit, date, pollDelay)
	collector.ConfigureNodesByStateGauge()
	collector.ConfigureExporterOpsMetrics()
	collector.ConfigureServiceUpdateMetrics()

	if enableContainers {
		collector.EnableContainersMetrics(true, containersIncludeSwarm)
		collector.ConfigureContainersStateGauge()
	} else {
		collector.EnableContainersMetrics(false, false)
	}
}

// startWorkers starts the reconciler, the event listener and the poller. The event-stream anchor
// is captured before the reconciler's first resync lists anything, so every change made while
// that resync runs reaches the reconciler as an event. The poller waits for that first resync.
func startWorkers(
	parentContext context.Context,
	waitGroup *sync.WaitGroup,
	dockerAPI collector.DockerAPI,
	delay time.Duration,
) {
	reconciler := collector.NewReconciler(dockerAPI)
	anchor := time.Now()

	waitGroup.Go(func() { reconciler.Run(parentContext) })
	startEventListener(parentContext, waitGroup, dockerAPI, reconciler, anchor)
	startPoller(parentContext, waitGroup, dockerAPI, delay, reconciler)
}

// startEventListener starts the goroutine that follows the event stream from anchor, handing
// every event to reconciler, until parentContext is done.
func startEventListener(
	parentContext context.Context,
	waitGroup *sync.WaitGroup,
	dockerAPI collector.DockerAPI,
	reconciler *collector.Reconciler,
	anchor time.Time,
) {
	waitGroup.Go(func() {
		listenErr := collector.ListenSwarmEvents(parentContext, dockerAPI, reconciler, anchor)
		if !errors.Is(listenErr, context.Canceled) {
			logger.L().Error("event listener exited with error", "err", listenErr)
		}
	})
}

// startPoller starts the goroutine that polls tasks (and containers, when enabled) every delay,
// against reconciler's polling snapshots. It waits for the reconciler's first resync first:
// before it the caches are empty, and there is nothing to poll.
func startPoller(
	parentContext context.Context,
	waitGroup *sync.WaitGroup,
	dockerAPI collector.DockerAPI,
	delay time.Duration,
	reconciler *collector.Reconciler,
) {
	waitGroup.Go(func() {
		loggerInstance := logger.L()

		select {
		case <-parentContext.Done():
			loggerInstance.Debug("polling loop: context canceled before the first resync completed")

			return
		case <-reconciler.Ready():
		}

		loggerInstance.Debug("start polling replicas state", "every", delay)

		ticker := time.NewTicker(delay)
		defer ticker.Stop()

		// --- Immediate first poll (no waiting for the first tick) ---
		pollOnce(parentContext, dockerAPI, reconciler)

		for {
			select {
			case <-parentContext.Done():
				loggerInstance.Debug("polling loop: context canceled")

				return
			case <-ticker.C:
				pollOnce(parentContext, dockerAPI, reconciler)
			}
		}
	})
}

// pollOnce runs one poll cycle: the replicas state, then the containers when enabled.
func pollOnce(
	parentContext context.Context,
	dockerAPI collector.DockerAPI,
	reconciler *collector.Reconciler,
) {
	loggerInstance := logger.L()
	startTime := time.Now()

	pollErr := collector.PollAndPublishReplicasState(parentContext, dockerAPI, reconciler)

	collector.ObservePollDuration(time.Since(startTime))
	collector.IncPolls()

	if pollErr != nil {
		collector.IncPollErrors()
		loggerInstance.Error("poll replicas state failed", "err", pollErr)
	}

	// --- Containers (opt-in) ---
	if *enableContainers {
		containerRows, contErr := collector.PollContainersState(parentContext, dockerAPI)
		if contErr != nil {
			loggerInstance.Warn("poll containers state failed", "err", contErr)
		} else {
			collector.UpdateContainersStateGauge(containerRows)
		}
	}
}

// runHTTPServer binds address and serves handler on it with serveHTTP.
func runHTTPServer(parentContext context.Context, address string, handler http.Handler) error {
	listener, listenErr := new(net.ListenConfig).Listen(parentContext, "tcp", address)
	if listenErr != nil {
		return fmt.Errorf("http listen: %w", listenErr)
	}

	return serveHTTP(parentContext, listener, handler)
}

// serveHTTP serves handler on listener until parentContext ends or the server fails, then shuts
// the server down gracefully. It takes ownership of listener.
func serveHTTP(parentContext context.Context, listener net.Listener, handler http.Handler) error {
	httpServer := &http.Server{
		Handler:           handler,
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       10 * time.Second,
		WriteTimeout:      15 * time.Second,
		IdleTimeout:       60 * time.Second,
	}

	errorChannel := make(chan error, 1)

	go func() {
		errorChannel <- httpServer.Serve(listener)
	}()

	var resultError error

	select {
	case resultError = <-errorChannel:
		// fallthrough to shutdown path
	case <-parentContext.Done():
		// context canceled: proceed to shutdown
	}

	// Graceful HTTP shutdown. Detached from parentContext on purpose: on SIGINT/SIGTERM it is
	// already done, and Shutdown given a done context returns at once instead of waiting for
	// in-flight scrapes.
	shutdownContext, shutdownCancel := context.WithTimeout(
		context.WithoutCancel(parentContext),
		httpShutdownTimeout,
	)
	defer shutdownCancel()

	shutdownErr := httpServer.Shutdown(shutdownContext)
	if shutdownErr != nil {
		logger.L().Warn("HTTP server shutdown", "err", shutdownErr)
	}

	return resultError
}
