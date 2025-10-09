// Package collector contains the Prometheus collectors and Swarm metadata helpers.
// This package is internal because its API is not intended to be imported by others.
package collector

import (
	"errors"
	"strconv"

	"github.com/docker/docker/api/types/swarm"
)

// ErrNoCachedMetadata is returned when a removed service is seen in events
// but we don't have cached labels/metadata for it (should be rare).
var ErrNoCachedMetadata = errors.New("no cached metadata found for removed service")

var (
	// customLabels is set once from main() to add user-specified service labels
	// (e.g., team, tier) as additional Prometheus label dimensions.
	customLabels []string

	// metadataCache stores service metadata (stack, service name, version, mode, custom labels)
	// keyed by Docker ServiceID to avoid repeated inspections.
	metadataCache = make(map[string]serviceMetadata)
)

// SetCustomLabels injects the list of label keys that should be propagated
// from Swarm service annotations into metric labels (sanitized on write).
func SetCustomLabels(ls []string) { customLabels = ls }

// serviceMetadata is immutable data we keep per service to populate metric labels.
type serviceMetadata struct {
	stack          string            // Docker stack name from label "com.docker.stack.namespace"
	service        string            // Service visible name (Annotations.Name)
	serviceVersion string            // Service object version (used to partition rollouts if needed)
	serviceMode    string            // "replicated" or "global"
	customLabels   map[string]string // arbitrary user-selected labels copied from service annotations
}

// buildMetadata constructs serviceMetadata from a Swarm service definition.
func buildMetadata(svc *swarm.Service) serviceMetadata {
	metadata := serviceMetadata{
		stack:          svc.Spec.Labels["com.docker.stack.namespace"],
		service:        svc.Spec.Name,
		serviceVersion: strconv.FormatUint(svc.Version.Index, 10),
		serviceMode:    serviceMode(svc),
		customLabels:   make(map[string]string),
	}

	for _, label := range customLabels {
		// Most service labels are under Spec.Annotations.Labels.
		if svc.Spec.Labels != nil {
			if val, ok := svc.Spec.Labels[label]; ok {
				metadata.customLabels[label] = val

				continue
			}
		}
		// Ensure the label exists even when absent on the service.
		metadata.customLabels[label] = ""
	}

	return metadata
}

// serviceMode returns the effective mode of a Swarm service as a string.
// This simplifies downstream label handling and Prometheus group-bys.
func serviceMode(svc *swarm.Service) string {
	if svc.Spec.Mode.Replicated != nil {
		return "replicated"
	}

	return "global"
}
