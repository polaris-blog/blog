package middleware

import (
	"bufio"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"time"
)

var skipPaths = []string{
	"/admin/static/",
	"/theme/static/",
	"/uploads/",
	"/favicon.ico",
}

func Logging(logger *slog.Logger) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			for _, prefix := range skipPaths {
				if strings.HasPrefix(r.URL.Path, prefix) {
					next.ServeHTTP(w, r)
					return
				}
			}

			start := time.Now()
			sw := &statusWriter{ResponseWriter: w, status: http.StatusOK}

			next.ServeHTTP(sw, r)

			if sw.status >= 400 {
				duration := time.Since(start)
				attrs := []slog.Attr{
					slog.Int("status", sw.status),
					slog.String("method", r.Method),
					slog.String("path", r.URL.Path),
					slog.Duration("latency", duration),
				}

				if sw.status >= 500 {
					logger.LogAttrs(r.Context(), slog.LevelError, "SERVER_ERROR", attrs...)
				} else {
					logger.LogAttrs(r.Context(), slog.LevelWarn, "CLIENT_ERROR", attrs...)
				}
			}
		})
	}
}

type statusWriter struct {
	http.ResponseWriter
	status int
}

func (w *statusWriter) WriteHeader(code int) {
	w.status = code
	w.ResponseWriter.WriteHeader(code)
}

func (w *statusWriter) Flush() {
	if f, ok := w.ResponseWriter.(http.Flusher); ok {
		f.Flush()
	}
}

func (w *statusWriter) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	if h, ok := w.ResponseWriter.(http.Hijacker); ok {
		return h.Hijack()
	}
	return nil, nil, http.ErrNotSupported
}
