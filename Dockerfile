FROM golang:1.23-alpine AS builder

RUN apk add --no-cache git

WORKDIR /src

COPY go.mod go.sum ./
RUN go mod download

COPY . .

RUN CGO_ENABLED=0 GOOS=linux go build -ldflags="-s -w" -o /polaris ./cmd/polaris

FROM alpine:3.21

RUN apk add --no-cache ca-certificates tzdata && \
    adduser -D -H -u 1000 polaris

COPY --from=builder /polaris /app/polaris

RUN mkdir -p /app/.polaris && chown -R polaris:polaris /app

USER polaris

WORKDIR /app

EXPOSE 8080

VOLUME ["/app/.polaris"]

ENTRYPOINT ["/app/polaris"]
