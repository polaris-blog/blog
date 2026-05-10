FROM golang:1.23-alpine AS builder

WORKDIR /app

RUN apk add --no-cache git

COPY go.mod go.sum ./
RUN go mod download

COPY . .

RUN go build -trimpath -ldflags "-s -w" -o polaris ./cmd/server

FROM alpine:latest

RUN apk --no-cache add ca-certificates tzdata

WORKDIR /app

COPY --from=builder /app/polaris .
COPY --from=builder /app/config.yaml .
COPY --from=builder /app/web ./web

RUN mkdir -p /app/uploads

EXPOSE 3000

CMD ["./polaris"]
