package http

import (
	"context"
	"log/slog"
	"net/http"
	"os"
	"time"

	"github.com/go-chi/chi/v5"
	chimw "github.com/go-chi/chi/v5/middleware"
	"github.com/polaris-blog/blog/internal/http/handler/admin"
	"github.com/polaris-blog/blog/internal/http/handler/web"
	"github.com/polaris-blog/blog/internal/http/middleware"
	"github.com/polaris-blog/blog/internal/i18n"
	"github.com/polaris-blog/blog/internal/plugin"
	"github.com/polaris-blog/blog/internal/repository"
	"github.com/polaris-blog/blog/internal/service"
	"github.com/polaris-blog/blog/internal/theme"
)

type Server struct {
	router          chi.Router
	httpServer      *http.Server
	postService     *service.PostService
	commentService  *service.CommentService
	authService     *service.AuthService
	categoryService *service.CategoryService
	tagService      *service.TagService
	themeManager    *theme.Manager
	pluginManager   *plugin.Manager
	options         repository.OptionRepository
	logger          *slog.Logger
	templateDir     string
	staticDir       string
	configDir       string
	uploadsDir      string
	i18nBundle      *i18n.Bundle
	pageHandler     *web.PageHandler
}

func NewServer(
	postService *service.PostService,
	commentService *service.CommentService,
	authService *service.AuthService,
	categoryService *service.CategoryService,
	tagService *service.TagService,
	themeManager *theme.Manager,
	pluginManager *plugin.Manager,
	options repository.OptionRepository,
	templateDir, staticDir, configDir, uploadsDir string,
	i18nBundle *i18n.Bundle,
	logger *slog.Logger,
) *Server {
	s := &Server{
		postService:     postService,
		commentService:  commentService,
		authService:     authService,
		categoryService: categoryService,
		tagService:      tagService,
		themeManager:    themeManager,
		pluginManager:   pluginManager,
		options:         options,
		templateDir:     templateDir,
		staticDir:       staticDir,
		configDir:       configDir,
		uploadsDir:      uploadsDir,
		i18nBundle:      i18nBundle,
		logger:          logger,
	}
	s.setupRoutes()
	return s
}

func (s *Server) setupRoutes() {
	r := chi.NewRouter()

	siteInfo := web.SiteInfo{
		Title:       "Polaris Blog",
		Description: "A dynamic blog powered by Polaris",
		URL:         "http://localhost:8080",
	}

	if s.themeManager != nil {
		s.pageHandler = web.NewPageHandler(s.themeManager, s.postService, s.commentService, s.categoryService, s.tagService, s.options, siteInfo, s.i18nBundle)
	}

	r.Use(chimw.RequestID)
	r.Use(chimw.RealIP)
	r.Use(middleware.Logging(s.logger))
	r.Use(chimw.Recoverer)
	r.Use(middleware.SecurityHeaders())
	r.Use(middleware.CacheControl())
	r.Use(chimw.Compress(4))

	r.Get("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`{"status":"ok"}`))
	})

	if s.themeManager != nil {
		staticDir := s.themeManager.StaticDir()
		if staticDir != "" {
			if _, err := os.Stat(staticDir); err == nil {
				r.Handle("/theme/static/*", http.StripPrefix("/theme/static/", http.FileServer(http.Dir(staticDir))))
			}
		}
	}

	if s.staticDir != "" {
		if _, err := os.Stat(s.staticDir); err == nil {
			r.Handle("/admin/static/*", http.StripPrefix("/admin/static/", http.FileServer(http.Dir(s.staticDir))))
		}
	}

	uploadsDir := s.uploadsDir
	if uploadsDir == "" {
		uploadsDir = "./uploads"
	}
	os.MkdirAll(uploadsDir, 0755)
	r.Handle("/uploads/*", http.StripPrefix("/uploads/", http.FileServer(http.Dir(uploadsDir))))

	if s.themeManager != nil && s.pageHandler != nil {
		pageHandler := s.pageHandler
		r.Get("/", pageHandler.Index)
		r.Get("/posts/{slug}", pageHandler.Post)
		r.Get("/archives", pageHandler.Archives)
		r.Get("/categories", pageHandler.Categories)
		r.Get("/categories/{slug}", pageHandler.CategoryDetail)
		r.Get("/tags", pageHandler.Tags)
		r.Get("/tags/{slug}", pageHandler.TagDetail)
		r.Get("/p/{slug}", pageHandler.CustomPage)
		r.Get("/feed.xml", pageHandler.RSS)
		r.Get("/sitemap.xml", pageHandler.Sitemap)

		r.NotFound(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			pageHandler.NotFound(w, r)
		}))
	} else {
		r.NotFound(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			http.Error(w, "Not Found", http.StatusNotFound)
		}))
	}

	webPostHandler := web.NewPostHandler(s.postService)
	r.Route("/api/v1", func(r chi.Router) {
		r.Get("/posts", webPostHandler.List)
		r.Get("/posts/{slug}", webPostHandler.GetBySlug)
		r.Get("/search", webPostHandler.Search)

		r.Post("/comments", web.NewCommentHandler(s.commentService).Create)
	})

	adminAPIPostHandler := admin.NewPostHandler(s.postService)
	adminAuthHandler := admin.NewAuthHandler(s.authService)
	commentAPIHandler := admin.NewCommentAPIHandler(s.commentService)
	categoryAPIHandler := admin.NewCategoryAPIHandler(s.categoryService)
	tagAPIHandler := admin.NewTagAPIHandler(s.tagService)
	themeAPIHandler := admin.NewThemeAPIHandler(s.themeManager)
	mediaAPIHandler := admin.NewMediaAPIHandler(s.uploadsDir)
	adminPageHandler := admin.NewAdminHandler(
		s.postService, s.commentService, s.authService,
		s.categoryService, s.tagService, s.themeManager,
		s.options,
		s.templateDir,
		s.i18nBundle,
	)

	r.Route("/api/admin", func(r chi.Router) {
		r.Post("/auth/login", adminAuthHandler.Login)
		r.Post("/auth/logout", adminAuthHandler.Logout)

		r.Group(func(r chi.Router) {
			r.Use(middleware.Auth(s.authService))
			r.Use(middleware.CSRF("/api/admin/auth/login", "/api/admin/auth/logout"))

			r.Get("/auth/me", adminAuthHandler.Me)

			r.Route("/posts", func(r chi.Router) {
				r.Get("/", adminAPIPostHandler.List)
				r.Post("/", s.invalidateCacheThen(adminAPIPostHandler.Create))
				r.Get("/{id}", adminAPIPostHandler.Get)
				r.Put("/{id}", s.invalidateCacheThen(adminAPIPostHandler.Update))
				r.Delete("/{id}", s.invalidateCacheThen(adminAPIPostHandler.Delete))
				r.Post("/{id}/publish", s.invalidateCacheThen(adminAPIPostHandler.Publish))
				r.Post("/{id}/unpublish", s.invalidateCacheThen(adminAPIPostHandler.Unpublish))
			})

			r.Route("/comments", func(r chi.Router) {
				r.Get("/", commentAPIHandler.List)
				r.Delete("/{id}", s.invalidateCacheThen(commentAPIHandler.Delete))
				r.Post("/{id}/approve", s.invalidateCacheThen(commentAPIHandler.Approve))
				r.Post("/{id}/spam", s.invalidateCacheThen(commentAPIHandler.Spam))
			})

			r.Post("/categories", s.invalidateCacheThen(categoryAPIHandler.Create))
			r.Delete("/categories/{id}", s.invalidateCacheThen(categoryAPIHandler.Delete))
			r.Post("/tags", s.invalidateCacheThen(tagAPIHandler.Create))
			r.Delete("/tags/{id}", s.invalidateCacheThen(tagAPIHandler.Delete))
			r.Post("/themes/{id}/activate", s.invalidateCacheThen(themeAPIHandler.Activate))
			r.Get("/themes/{id}/settings", themeAPIHandler.GetSettings)
			r.Put("/themes/{id}/settings", s.invalidateCacheThen(themeAPIHandler.UpdateSettings))
			r.Post("/themes/upload", s.invalidateCacheThen(themeAPIHandler.Upload))
			r.Delete("/themes/{id}", s.invalidateCacheThen(themeAPIHandler.Delete))

			r.Post("/media/upload", mediaAPIHandler.Upload)
			r.Get("/media/list", mediaAPIHandler.List)

			pluginAPIHandler := admin.NewPluginAPIHandler(s.pluginManager)
			r.Get("/plugins", pluginAPIHandler.List)
			r.Post("/plugins/upload", s.invalidateCacheThen(pluginAPIHandler.Upload))
			r.Delete("/plugins/{id}", s.invalidateCacheThen(pluginAPIHandler.Delete))
			r.Get("/plugins/{id}/settings", pluginAPIHandler.GetSettings)
			r.Put("/plugins/{id}/settings", s.invalidateCacheThen(pluginAPIHandler.UpdateSettings))

			r.Put("/settings", s.invalidateCacheThen(adminPageHandler.UpdateSettings))
			r.Get("/nav", adminPageHandler.GetNavItems)
			r.Put("/nav", s.invalidateCacheThen(adminPageHandler.UpdateNavItems))
			r.Post("/pages", s.invalidateCacheThen(adminPageHandler.CreatePage))
			r.Put("/pages/{id}", s.invalidateCacheThen(adminPageHandler.UpdatePage))
			r.Delete("/pages/{id}", s.invalidateCacheThen(adminAPIPostHandler.Delete))
		})
	})

	r.Get("/admin/login", adminPageHandler.LoginPage)
	r.Post("/admin/auth/logout", func(w http.ResponseWriter, r *http.Request) {
		http.SetCookie(w, &http.Cookie{Name: "access_token", Value: "", Path: "/", MaxAge: -1})
		http.Redirect(w, r, "/admin/login", http.StatusSeeOther)
	})

	r.Route("/admin", func(r chi.Router) {
		r.Use(middleware.Auth(s.authService))

		r.Get("/", adminPageHandler.Dashboard)
		r.Get("/posts", adminPageHandler.PostsList)
		r.Get("/posts/new", adminPageHandler.PostNew)
		r.Get("/posts/{id}/edit", adminPageHandler.PostEdit)
		r.Get("/pages", adminPageHandler.PagesList)
		r.Get("/pages/new", adminPageHandler.PageNew)
		r.Get("/pages/{id}/edit", adminPageHandler.PageEdit)
		r.Get("/navigation", adminPageHandler.NavigationPage)
		r.Get("/categories", adminPageHandler.CategoriesList)
		r.Get("/tags", adminPageHandler.TagsList)
		r.Get("/comments", adminPageHandler.CommentsList)
		r.Get("/media", adminPageHandler.MediaList)
		r.Get("/themes", adminPageHandler.ThemesList)
		r.Get("/plugins", adminPageHandler.PluginsList)
		r.Get("/settings", adminPageHandler.SettingsPage)
	})

	s.router = r
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if s.pageHandler != nil && s.pageHandler.TryServeCached(w, r) {
		return
	}
	s.router.ServeHTTP(w, r)
}

func (s *Server) ListenAndServe(addr string) error {
	s.logger.Info("starting server", slog.String("addr", addr))
	s.httpServer = &http.Server{
		Addr:           addr,
		Handler:        s,
		ReadTimeout:    10 * time.Second,
		WriteTimeout:   15 * time.Second,
		IdleTimeout:    120 * time.Second,
		MaxHeaderBytes: 1 << 20,
	}
	return s.httpServer.ListenAndServe()
}

func (s *Server) Shutdown(ctx context.Context) error {
	if s.httpServer != nil {
		return s.httpServer.Shutdown(ctx)
	}
	return nil
}

func (s *Server) invalidateCacheThen(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		next(w, r)
		if s.pageHandler != nil {
			s.pageHandler.InvalidateHTMLCache()
		}
	}
}
