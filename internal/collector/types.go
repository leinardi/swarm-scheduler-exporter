package collector

import (
	"errors"
	"strconv"

	"github.com/docker/docker/api/types/swarm"
)

// ErrNoCachedMetadata is returned when a removed service has no cached metadata.
var ErrNoCachedMetadata = errors.New("no cached metadata found for removed service")

var (
	// set by main() after flags parsed.
	customLabels []string

	// Prometheus exporter pattern: package singletons for caches/collectors.
	metadataCache = make(map[string]serviceMetadata)
)

func SetCustomLabels(ls []string) { customLabels = ls }

type serviceMetadata struct {
	stack          string
	service        string
	serviceVersion string
	serviceMode    string
	customLabels   map[string]string
}

// buildMetadata builds a serviceMetadata from a *swarm.Service.
// Accept a pointer to avoid copying the large swarm.Service value.
func buildMetadata(svc *swarm.Service) serviceMetadata {
	metadata := serviceMetadata{
		stack:          svc.Spec.Labels["com.docker.stack.namespace"],
		service:        svc.Spec.Name,
		serviceVersion: strconv.FormatUint(svc.Version.Index, 10),
		serviceMode:    serviceMode(svc),
		customLabels:   make(map[string]string),
	}

	for _, label := range customLabels {
		if val, ok := svc.Spec.Labels[label]; ok {
			metadata.customLabels[label] = val
		} else {
			metadata.customLabels[label] = ""
		}
	}

	return metadata
}

// serviceMode returns "replicated" or "global" for the provided service.
// Accept a pointer to avoid copying the large swarm.Service value.
func serviceMode(svc *swarm.Service) string {
	if svc.Spec.Mode.Replicated != nil {
		return "replicated"
	}

	return "global"
}
