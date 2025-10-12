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

// customLabelDef pairs the raw service label key with its sanitized Prometheus label name.
type customLabelDef struct {
	raw       string
	sanitized string
}

// serviceMetadata is immutable data we keep per service to populate metric labels.
type serviceMetadata struct {
	stack           string            // Docker stack name from label "com.docker.stack.namespace"
	service         string            // Service visible name (Annotations.Name)
	serviceMode     string            // "replicated" or "global"
	customLabels    map[string]string // key: sanitized name; value: service label value
	desiredReplicas float64           // last computed desired replicas for this service
}

// --- Package-level state (protected by locks) ---

var (
	// metadataCache stores service metadata (stack, service name, mode, custom labels)
	// keyed by Docker ServiceID. Protected by metadataMu.
	metadataMu    sync.RWMutex
	metadataCache = make(map[string]serviceMetadata)
)

// Cached nodes (latest snapshot) with a lock for concurrent access.
var (
	nodesMu     sync.RWMutex
	cachedNodes []swarm.Node
)

// customLabelDefs holds the list of user-requested custom labels as raw+sanitized pairs.
var customLabelDefs []customLabelDef

// --- Nodes snapshot management ---

// setCachedNodes replaces the node snapshot.
func setCachedNodes(nodes []swarm.Node) {
	nodesMu.Lock()
	defer nodesMu.Unlock()

	// Copy to avoid sharing memory with the Docker client slice.
	dst := make([]swarm.Node, len(nodes))
	copy(dst, nodes)
	cachedNodes = dst
}

// getCachedNodes returns a copy of the last cached nodes (may be nil).
func getCachedNodes() []swarm.Node {
	nodesMu.RLock()
	defer nodesMu.RUnlock()

	if len(cachedNodes) == 0 {
		return nil
	}

	dst := make([]swarm.Node, len(cachedNodes))
	copy(dst, cachedNodes)

	return dst
}

// --- Service metadata helpers ---

// getAllServiceIDs returns a stable copy of all known service IDs from the metadata cache.
func getAllServiceIDs() []string {
	metadataMu.RLock()
	defer metadataMu.RUnlock()

	if len(metadataCache) == 0 {
		return nil
	}

	ids := make([]string, 0, len(metadataCache))
	for serviceID := range metadataCache {
		ids = append(ids, serviceID)
	}

	return ids
}

// getGlobalServiceIDs returns only global-mode service IDs from the cache.
func getGlobalServiceIDs() []string {
	metadataMu.RLock()
	defer metadataMu.RUnlock()

	var ids []string

	for serviceID, md := range metadataCache {
		if md.serviceMode == "global" {
			ids = append(ids, serviceID)
		}
	}

	return ids
}

// SetCustomLabels records both the raw keys (as they appear in Swarm) and their sanitized names.
// rawKeys and sanitizedKeys must have the same length and aligned order.
func SetCustomLabels(rawKeys, sanitizedKeys []string) {
	defs := make([]customLabelDef, 0, len(rawKeys))
	for index := range rawKeys {
		defs = append(defs, customLabelDef{
			raw:       rawKeys[index],
			sanitized: sanitizedKeys[index],
		})
	}

	customLabelDefs = defs
}

// getSanitizedCustomLabelNames returns the sanitized label names for metric definitions.
func getSanitizedCustomLabelNames() []string {
	out := make([]string, 0, len(customLabelDefs))
	for index := range customLabelDefs {
		out = append(out, customLabelDefs[index].sanitized)
	}

	return out
}

// buildMetadata constructs serviceMetadata from a Swarm service definition.
func buildMetadata(svc *swarm.Service) serviceMetadata {
	metadata := serviceMetadata{
		stack:        svc.Spec.Labels["com.docker.stack.namespace"],
		service:      svc.Spec.Name,
		serviceMode:  serviceMode(svc),
		customLabels: make(map[string]string, len(customLabelDefs)),
	}

	for index := range customLabelDefs {
		rawKey := customLabelDefs[index].raw
		sanitizedKey := customLabelDefs[index].sanitized

		if svc.Spec.Labels != nil {
			if value, ok := svc.Spec.Labels[rawKey]; ok {
				metadata.customLabels[sanitizedKey] = value

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
	defer metadataMu.Unlock()

	metadataCache[serviceID] = metadata
}

func getServiceMetadata(serviceID string) (serviceMetadata, bool) {
	metadataMu.RLock()
	defer metadataMu.RUnlock()

	metadata, ok := metadataCache[serviceID]

	return metadata, ok
}

func deleteServiceMetadata(serviceID string) {
	metadataMu.Lock()
	defer metadataMu.Unlock()

	delete(metadataCache, serviceID)
}

func getServiceModeCached(serviceID string) (string, bool) {
	metadataMu.RLock()
	defer metadataMu.RUnlock()

	metadata, ok := metadataCache[serviceID]
	if !ok {
		return "", false
	}

	return metadata.serviceMode, true
}

// setServiceDesiredReplicas updates the cached desired replicas for a service.
func setServiceDesiredReplicas(serviceID string, desired float64) {
	metadataMu.Lock()
	defer metadataMu.Unlock()

	metadata, ok := metadataCache[serviceID]
	if !ok {
		// Unknown service; nothing to update.
		return
	}

	metadata.desiredReplicas = desired
	metadataCache[serviceID] = metadata
}

// getServiceDesiredReplicas returns the last cached desired replicas for a service.
func getServiceDesiredReplicas(serviceID string) (float64, bool) {
	metadataMu.RLock()
	defer metadataMu.RUnlock()

	md, ok := metadataCache[serviceID]
	if !ok {
		return 0, false
	}

	return md.desiredReplicas, true
}
