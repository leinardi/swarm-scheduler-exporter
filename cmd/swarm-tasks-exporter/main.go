package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"os"
	"time"

	"github.com/docker/docker/client"
	"github.com/leinardi/swarm-tasks-exporter/internal/collector"
	"github.com/leinardi/swarm-tasks-exporter/internal/server"
	"github.com/sirupsen/logrus"
)

func configureLogger() {
	switch *logFormat {
	case "text":
		logrus.SetFormatter(new(logrus.TextFormatter))
	case "json":
		logrus.SetFormatter(new(logrus.JSONFormatter))
	default:
		fmt.Fprintf(os.Stderr, "Invalid log format %q. Should be either json or text.", *logFormat)
		os.Exit(1)
	}

	switch *logLevel {
	case "debug":
		logrus.SetLevel(logrus.DebugLevel)
	case "info":
		logrus.SetLevel(logrus.InfoLevel)
	case "warn":
		logrus.SetLevel(logrus.WarnLevel)
	case "error":
		logrus.SetLevel(logrus.ErrorLevel)
	case "fatal":
		logrus.SetLevel(logrus.FatalLevel)
	case "panic":
		logrus.SetLevel(logrus.PanicLevel)
	default:
		fmt.Fprintf(
			os.Stderr,
			"Invalid log level %q. Should be either debug, info, warn, error, fatal, panic.",
			*logLevel,
		)
		os.Exit(1)
	}
}

type stringSlice []string

func (i *stringSlice) String() string {
	return fmt.Sprint(*i)
}

func (i *stringSlice) Set(value string) error {
	if len(value) == 0 {
		return errors.New("empty flag value")
	}

	*i = append(*i, value)

	return nil
}

var (
	listenAddr = flag.String("listen-addr", "0.0.0.0:8888", "IP address and port to bind")
	pollDelay  = flag.Duration("poll-delay", 10*time.Second, "Delay in seconds between two polls")
	logFormat  = flag.String("log-format", "text", "Either json or text")
	logLevel   = flag.String("log-level", "info", "Either debug, info, warn, error, fatal, panic")
	help       = flag.Bool("help", false, "Display help message")

	customLabels stringSlice
)

func usage() {
	fmt.Printf("Usage of %s:\n", os.Args[0])
	flag.PrintDefaults()
}

func main() {
	flag.Var(&customLabels, "label", "Name of custom service labels to add to metrics")
	flag.Parse()

	if *help {
		usage()
		os.Exit(0)
	}

	configureLogger()
	collector.SetCustomLabels([]string(customLabels))
	collector.ConfigureDesiredReplicasGauge()
	collector.ConfigureReplicasStateGauge()

	ctx := context.Background()

	cli, err := client.NewClientWithOpts(client.FromEnv)
	if err != nil {
		logrus.Fatal(err)
	}

	defer cli.Close()

	cli.NegotiateAPIVersion(ctx)

	go func() {
		err := collector.InitDesiredReplicasGauge(ctx, cli)
		if err != nil {
			logrus.Fatal(err)
		}

		err = collector.ListenSwarmEvents(ctx, cli)
		if err != nil {
			logrus.Fatal(err)
		}
	}()

	go func() {
		logrus.Info("Start polling replicas state every ", *pollDelay)

		for {
			logrus.Info("Polling replicas state...")

			polled, err := collector.PollReplicasState(ctx, cli)
			if err != nil {
				logrus.Error(err)
			}

			collector.UpdateReplicasStateGauge(polled)
			time.Sleep(*pollDelay)
		}
	}()

	mux := server.NewMux()

	logrus.Infof("Start HTTP server on %q.", *listenAddr)

	if err := http.ListenAndServe(*listenAddr, mux); err != nil {
		logrus.Fatal(err)
	}
}
