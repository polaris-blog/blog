FROM alpine:3.21

ARG TARGETARCH

RUN apk add --no-cache ca-certificates tzdata && \
    adduser -D -H -u 1000 polaris

COPY binaries/${TARGETARCH}/polaris /app/polaris

RUN mkdir -p /app/.polaris && chown -R polaris:polaris /app

USER polaris

WORKDIR /app

EXPOSE 8080

VOLUME ["/app/.polaris"]

ENTRYPOINT ["/app/polaris"]
