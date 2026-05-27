package middleware

import (
	"net/http"
	"strings"
)

func CacheControl() func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			path := r.URL.Path

			if strings.HasPrefix(path, "/admin/static/") || strings.HasPrefix(path, "/theme/static/") {
				if strings.Contains(path, ".") {
					ext := path[strings.LastIndex(path, "."):]
					switch ext {
					case ".js", ".css", ".woff", ".woff2", ".ttf", ".eot", ".otf":
						w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
					case ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".ico":
						w.Header().Set("Cache-Control", "public, max-age=86400")
					default:
						w.Header().Set("Cache-Control", "public, max-age=3600")
					}
				}
			} else if strings.HasPrefix(path, "/uploads/") {
				w.Header().Set("Cache-Control", "public, max-age=86400")
			}

			next.ServeHTTP(w, r)
		})
	}
}
