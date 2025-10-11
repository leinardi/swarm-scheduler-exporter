FROM golang:1.10-alpine as builder

ENV SRC_DIR ${GOPATH}/src/NiR-/swarm-scheduler-exporter/

RUN apk add --no-cache ca-certificates git

COPY . ${SRC_DIR}
WORKDIR ${SRC_DIR}

RUN go get -d -v

RUN CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -o /go/bin/swarm-scheduler-exporter

###############################################################################

FROM scratch

COPY --from=builder /go/bin/swarm-scheduler-exporter /go/bin/swarm-scheduler-exporter

ENTRYPOINT ["/go/bin/swarm-scheduler-exporter"]
