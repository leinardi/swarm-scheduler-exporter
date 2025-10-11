// Package collector contains the Prometheus collectors and Swarm metadata helpers.
// This package is internal because its API is not intended to be imported by others.
package collector

import (
	"errors"
	"sync"

	"github.com/docker/docker/api/types/swarm"
)

// ErrNoCachedMetadata is returned when a removed service is seen in events
// but we don't have cached labels/metadata for it (should be rare).
var ErrNoCachedMetadata = errors.New("no cached metadata found for removed service")

var (
	// metadataCache stores service metadata (stack, service name, mode, custom labels)
	// keyed by Docker ServiceID. Protected by metadataMu.
	metadataMu    sync.RWMutex
	metadataCache = make(map[string]serviceMetadata)
)

// customLabelDef pairs the raw service label key with its sanitized Prometheus label name.
type customLabelDef struct {
	raw       string
	sanitized string
}

// customLabelDefs holds the list of user-requested custom labels as raw+sanitized pairs.
var customLabelDefs []customLabelDef

// SetCustomLabels records both the raw keys (as they appear in Swarm) and their sanitized names.
// rawKeys and sanitizedKeys must have the same length and aligned order.
func SetCustomLabels(rawKeys, sanitizedKeys []string) {
	defs := make([]customLabelDef, 0, len(rawKeys))
	for i := range rawKeys {
		defs = append(defs, customLabelDef{
			raw:       rawKeys[i],
			sanitized: sanitizedKeys[i],
		})
	}

	customLabelDefs = defs
}

// getSanitizedCustomLabelNames returns the sanitized label names for metric definitions.
func getSanitizedCustomLabelNames() []string {
	out := make([]string, 0, len(customLabelDefs))
	for i := range customLabelDefs {
		out = append(out, customLabelDefs[i].sanitized)
	}

	return out
}

// serviceMetadata is immutable data we keep per service to populate metric labels.
type serviceMetadata struct {
	stack        string            // Docker stack name from label "com.docker.stack.namespace"
	service      string            // Service visible name (Annotations.Name)
	serviceMode  string            // "replicated" or "global"
	customLabels map[string]string // key: sanitized name; value: service label value
}

// buildMetadata constructs serviceMetadata from a Swarm service definition.
func buildMetadata(svc *swarm.Service) serviceMetadata {
	metadata := serviceMetadata{
		stack:        svc.Spec.Labels["com.docker.stack.namespace"],
		service:      svc.Spec.Name,
		serviceMode:  serviceMode(svc),
		customLabels: make(map[string]string, len(customLabelDefs)),
	}

	for i := range customLabelDefs {
		rawKey := customLabelDefs[i].raw
		sanitizedKey := customLabelDefs[i].sanitized

		if svc.Spec.Labels != nil {
			if val, ok := svc.Spec.Labels[rawKey]; ok {
				metadata.customLabels[sanitizedKey] = val

				continue
			}
		}
		// Ensure the label exists even when absent on the service (exhaustive emission).
		metadata.customLabels[sanitizedKey] = ""
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

// --- Synchronized accessors for metadataCache ---

func setServiceMetadata(serviceID string, metadata serviceMetadata) {
	metadataMu.Lock()

	metadataCache[serviceID] = metadata

	metadataMu.Unlock()
}

func getServiceMetadata(serviceID string) (serviceMetadata, bool) {
	metadataMu.RLock()

	metadata, ok := metadataCache[serviceID]

	metadataMu.RUnlock()

	return metadata, ok
}

func deleteServiceMetadata(serviceID string) {
	metadataMu.Lock()
	delete(metadataCache, serviceID)
	metadataMu.Unlock()
}

func getServiceModeCached(serviceID string) (string, bool) {
	metadataMu.RLock()

	metadata, ok := metadataCache[serviceID]

	metadataMu.RUnlock()

	if !ok {
		return "", false
	}

	return metadata.serviceMode, true
}
