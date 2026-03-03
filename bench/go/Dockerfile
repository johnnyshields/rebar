FROM golang:1.24-bookworm AS builder
WORKDIR /app
COPY bench/go/ ./
RUN go mod download || true
RUN CGO_ENABLED=0 go build -o /gateway ./gateway
RUN CGO_ENABLED=0 go build -o /compute ./compute
RUN CGO_ENABLED=0 go build -o /store ./store

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /gateway /usr/local/bin/gateway
COPY --from=builder /compute /usr/local/bin/compute
COPY --from=builder /store /usr/local/bin/store
CMD ["gateway"]
