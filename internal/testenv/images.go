//go:build integration

/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
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

package testenv

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"strings"

	"github.com/moby/moby/api/types/jsonstream"
	dockerclient "github.com/moby/moby/client"
)

// WorkloadImage is the one image every test service runs. It is digest-pinned so every run loads
// the same bytes, and it is pulled only on the host: the nodes receive it through ImageLoad and
// services reference it by image ID, which makes Swarm's executor skip the pull entirely. The nodes
// never contact a registry, so there are no rate limits or network flakes inside the cluster.
const WorkloadImage = "busybox:1.37.0@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"

const (
	loadedImageIDPrefix  = "Loaded image ID: "
	loadedImageRefPrefix = "Loaded image: "
)

// loadWorkloadImage copies WorkloadImage from the host to every node and returns its image ID,
// which must be the same on every node.
func loadWorkloadImage(
	ctx context.Context,
	hostClient *dockerclient.Client,
	nodes []Node,
	log *slog.Logger,
) (string, error) {
	err := ensureHostImage(ctx, hostClient, WorkloadImage, log)
	if err != nil {
		return "", err
	}

	archivePath, err := saveImageToFile(ctx, hostClient, WorkloadImage)
	if err != nil {
		return "", err
	}
	defer os.Remove(archivePath)

	imageID := ""

	for idx := range nodes {
		nodeImageID, loadErr := loadImageOnNode(ctx, &nodes[idx], archivePath)
		if loadErr != nil {
			return "", loadErr
		}

		if imageID != "" && nodeImageID != imageID {
			return "", fmt.Errorf(
				"%s has %s, %s has %s: %w",
				nodes[0].Name,
				imageID,
				nodes[idx].Name,
				nodeImageID,
				errImageIDMismatch,
			)
		}

		imageID = nodeImageID
	}

	log.Info(
		"workload image loaded on every node",
		slog.String("image", WorkloadImage),
		slog.String("id", imageID),
	)

	return imageID, nil
}

// saveImageToFile writes the image archive to a temporary file, so each node can be fed a fresh
// reader instead of the whole archive being held in memory. The caller removes the file.
func saveImageToFile(
	ctx context.Context,
	hostClient *dockerclient.Client,
	imageRef string,
) (_ string, retErr error) {
	archive, err := os.CreateTemp("", "sse-it-workload-*.tar")
	if err != nil {
		return "", fmt.Errorf("create image archive: %w", err)
	}

	defer func() {
		closeErr := archive.Close()
		if retErr == nil && closeErr != nil {
			retErr = fmt.Errorf("close image archive: %w", closeErr)
		}

		if retErr != nil {
			_ = os.Remove(archive.Name())
		}
	}()

	saveCtx, cancel := context.WithTimeout(ctx, imageTransferTimeout)
	defer cancel()

	saved, err := hostClient.ImageSave(saveCtx, []string{imageRef})
	if err != nil {
		return "", fmt.Errorf("save host image %q: %w", imageRef, err)
	}
	defer saved.Close()

	_, err = io.Copy(archive, saved)
	if err != nil {
		return "", fmt.Errorf("write image archive for %q: %w", imageRef, err)
	}

	return archive.Name(), nil
}

// loadImageOnNode loads the archive into the node's daemon and returns the loaded image's ID, as
// confirmed by ImageInspect on that node.
func loadImageOnNode(ctx context.Context, node *Node, archivePath string) (string, error) {
	archive, err := os.Open(archivePath)
	if err != nil {
		return "", fmt.Errorf("open image archive: %w", err)
	}
	defer archive.Close()

	var imageID string

	err = withNodeClient(node, func(cli *dockerclient.Client) error {
		loadedRef, loadErr := loadArchive(ctx, cli, archive)
		if loadErr != nil {
			return loadErr
		}

		inspected, inspectErr := withTimeout(
			ctx,
			opTimeout,
			func(opCtx context.Context) (dockerclient.ImageInspectResult, error) {
				return cli.ImageInspect(opCtx, loadedRef)
			},
		)
		if inspectErr != nil {
			return fmt.Errorf("inspect loaded image %q: %w", loadedRef, inspectErr)
		}

		imageID = inspected.ID

		return nil
	})
	if err != nil {
		return "", fmt.Errorf("load workload image on %q: %w", node.Name, err)
	}

	return imageID, nil
}

// loadArchive sends archive to the daemon, then drains and closes the response: the load is only
// complete once the daemon has written its whole response. It returns the reference the daemon
// reports as loaded.
func loadArchive(ctx context.Context, cli *dockerclient.Client, archive io.Reader) (string, error) {
	loadCtx, cancel := context.WithTimeout(ctx, imageTransferTimeout)
	defer cancel()

	loaded, err := cli.ImageLoad(loadCtx, archive, dockerclient.ImageLoadWithQuiet(true))
	if err != nil {
		return "", fmt.Errorf("image load: %w", err)
	}

	loadedRef, parseErr := parseLoadResponse(loaded)
	_, drainErr := io.Copy(io.Discard, loaded)
	closeErr := loaded.Close()

	err = errors.Join(parseErr, drainErr, closeErr)
	if err != nil {
		return "", fmt.Errorf("image load response: %w", err)
	}

	return loadedRef, nil
}

// parseLoadResponse reads the JSON message stream of an image load and returns the loaded image,
// preferring the image ID over a tag when the daemon reports both.
func parseLoadResponse(body io.Reader) (string, error) {
	decoder := json.NewDecoder(body)
	loadedID := ""
	loadedRef := ""

	for {
		var msg jsonstream.Message

		err := decoder.Decode(&msg)
		if errors.Is(err, io.EOF) {
			break
		}

		if err != nil {
			return "", fmt.Errorf("decode: %w", err)
		}

		if msg.Error != nil {
			return "", fmt.Errorf("%w: %s", errWorkloadLoadFail, msg.Error.Message)
		}

		line := strings.TrimSpace(msg.Stream)

		if id, found := strings.CutPrefix(line, loadedImageIDPrefix); found {
			loadedID = id
		} else if ref, found := strings.CutPrefix(line, loadedImageRefPrefix); found {
			loadedRef = ref
		}
	}

	if loadedID != "" {
		return loadedID, nil
	}

	if loadedRef != "" {
		return loadedRef, nil
	}

	return "", errImageNotLoaded
}
