package middleware

import (
	"net/http"
	"strings"
)

func CORS(allowedOrigins []string) func(http.Handler) http.Handler {
	origins := make(map[string]bool, len(allowedOrigins))
	for _, o := range allowedOrigins {
		origins[o] = true
	}

	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if len(origins) == 0 {
				next.ServeHTTP(w, r)
				return
			}

			origin := r.Header.Get("Origin")

			if origins[origin] {
				w.Header().Set("Access-Control-Allow-Origin", origin)
				w.Header().Set("Vary", "Origin")
				w.Header().Set("Access-Control-Allow-Credentials", "true")
			}

			w.Header().Set("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, PATCH, OPTIONS")
			w.Header().Set("Access-Control-Allow-Headers", "Content-Type, Authorization, X-Requested-With")
			w.Header().Set("Access-Control-Max-Age", "86400")

			if r.Method == http.MethodOptions {
				w.WriteHeader(http.StatusNoContent)
				return
			}

			next.ServeHTTP(w, r)
		})
	}
}

func CSRF(skipPaths ...string) func(http.Handler) http.Handler {
	skipSet := make(map[string]bool, len(skipPaths))
	for _, p := range skipPaths {
		skipSet[p] = true
	}

	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.Method == http.MethodGet || r.Method == http.MethodHead || r.Method == http.MethodOptions {
				next.ServeHTTP(w, r)
				return
			}

			if skipSet[r.URL.Path] {
				next.ServeHTTP(w, r)
				return
			}

			referer := r.Header.Get("Referer")
			origin := r.Header.Get("Origin")

			if origin == "" && referer == "" {
				next.ServeHTTP(w, r)
				return
			}

			hosts := []string{r.Host}
			if fh := r.Header.Get("X-Forwarded-Host"); fh != "" {
				hosts = append(hosts, fh)
			}
			if fh := r.Header.Get("X-Original-Host"); fh != "" {
				hosts = append(hosts, fh)
			}
			if fh := r.Header.Get("Forwarded"); fh != "" {
				for _, part := range strings.Split(fh, ";") {
					part = strings.TrimSpace(part)
					if strings.HasPrefix(part, "host=") {
						hosts = append(hosts, strings.TrimPrefix(part, "host="))
					}
				}
			}

			for _, host := range hosts {
				if strings.HasPrefix(origin, "http://"+host) || strings.HasPrefix(origin, "https://"+host) {
					next.ServeHTTP(w, r)
					return
				}
				if strings.HasPrefix(referer, "http://"+host) || strings.HasPrefix(referer, "https://"+host) {
					next.ServeHTTP(w, r)
					return
				}
			}

			http.Error(w, "csrf validation failed", http.StatusForbidden)
		})
	}
}
