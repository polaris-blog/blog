package server

import (
	"bytes"
	"context"
	"fmt"
	"html/template"
	"regexp"
	"strings"
	"time"

	"github.com/gofiber/fiber/v2"
	"github.com/gofiber/fiber/v2/middleware/compress"
	"github.com/gofiber/fiber/v2/middleware/cors"
	"github.com/gofiber/fiber/v2/middleware/limiter"
	"github.com/gofiber/fiber/v2/middleware/logger"
	"github.com/gofiber/fiber/v2/middleware/recover"
	"github.com/gofiber/fiber/v2/middleware/session"
	"github.com/gofiber/template/html/v2"
	"github.com/polaris/blog/internal/config"
	"github.com/polaris/blog/internal/handlers"
	"github.com/polaris/blog/internal/middleware"
	"github.com/polaris/blog/internal/models"
	"github.com/polaris/blog/internal/services"
	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/extension"
	gmhtml "github.com/yuin/goldmark/renderer/html"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type Server struct {
	app         *fiber.App
	cfg         *config.Config
	db          *gorm.DB
	logger      *zap.Logger
	authService *services.AuthService

	postService      *services.PostService
	categoryService  *services.CategoryService
	tagService       *services.TagService
	commentService   *services.CommentService
	settingService   *services.SettingService
	navigationService *services.NavigationService
	pageService      *services.PageService
	themeService     *services.ThemeService
	themeViews       *html.Engine
	systemViews      *html.Engine // admin/login — always from web/templates/
	pluginService    *services.PluginService
}

func New(cfg *config.Config, db *gorm.DB, logger *zap.Logger) *Server {
	// Initialize theme service first to determine template path
	themeService := services.NewThemeService(db, cfg, logger)
	themeService.EnsureDefaultTheme()

	templatePath := themeService.GetTemplatePath()

	// Theme-specific engine for public pages
	engine := html.New(templatePath, ".html")
	engine.Reload(true)
	engine.Debug(true)
	registerTemplateFuncs(engine)

	// System engine for admin/login (always from web/templates/, not themeable)
	systemEngine := html.New("./web/templates", ".html")
	systemEngine.Reload(true)
	systemEngine.Debug(true)
	registerTemplateFuncs(systemEngine)

	app := fiber.New(fiber.Config{
		AppName:      "Polaris Blog",
		ServerHeader: "Polaris",
		ReadTimeout:  cfg.Server.ReadTimeout,
		WriteTimeout: cfg.Server.WriteTimeout,
		Views:        engine,
	})

	postService := services.NewPostService(db, logger)
	categoryService := services.NewCategoryService(db, logger)
	tagService := services.NewTagService(db, logger)
	commentService := services.NewCommentService(db, logger)
	settingService := services.NewSettingService(db, logger)
	navigationService := services.NewNavigationService(db, logger)
	navigationService.SeedBuiltins()
	pageService := services.NewPageService(db, logger)

	return &Server{
		app:    app,
		cfg:    cfg,
		db:     db,
		logger: logger,
		
		postService:     postService,
		categoryService: categoryService,
		tagService:      tagService,
		commentService:  commentService,
		settingService:  settingService,
		navigationService: navigationService,
		pageService:     pageService,
		themeService:    themeService,
		themeViews:      engine,
		systemViews:     systemEngine,
	}
}

func (s *Server) setupMiddleware() {
	// Initialize pluginService early (needed by middleware and render)
	s.pluginService = services.NewPluginService(s.db, s.cfg, s.logger)

	s.app.Use(recover.New())
	s.app.Use(logger.New())

	// Redirect to /install if system not initialized (no admin user)
	s.app.Use(func(c *fiber.Ctx) error {
		path := c.Path()
		// Allow install page, install API, static assets, and feed endpoints through
		if path == "/install" || path == "/rss" || path == "/sitemap.xml" ||
			strings.HasPrefix(path, "/api/install") ||
			strings.HasPrefix(path, "/api/admin/init") ||
			strings.HasPrefix(path, "/uploads") || strings.HasPrefix(path, "/assets") {
			return c.Next()
		}
		// Check if admin exists
		var count int64
		s.db.Model(&models.User{}).Where("role = ?", "admin").Count(&count)
		if count == 0 {
			return c.Redirect("/install")
		}
		return c.Next()
	})
	s.app.Use(middleware.ChallengeMiddleware(s.pluginService))
	s.app.Use(compress.New(compress.Config{
		Level: compress.LevelBestSpeed,
	}))
	s.app.Use(cors.New(cors.Config{
		AllowOrigins:     s.cfg.Server.CORSAllowOrigins,
		AllowMethods:     "GET,POST,PUT,DELETE,OPTIONS",
		AllowHeaders:     "Origin,Content-Type,Accept,Authorization,X-CSRF-Token",
		AllowCredentials: false,
	}))
	s.app.Use(func(c *fiber.Ctx) error {
		c.Set("X-Content-Type-Options", "nosniff")
		c.Set("X-Frame-Options", "DENY")
		c.Set("X-XSS-Protection", "1; mode=block")
		c.Set("Referrer-Policy", "strict-origin-when-cross-origin")
		return c.Next()
	})
}

func (s *Server) setupRoutes() {
	store := session.New()
	authService := services.NewAuthService(s.db, s.cfg, s.logger)
	s.authService = authService
	mediaService := services.NewMediaService(s.db, s.cfg, s.logger)
	searchService := services.NewSearchService(s.db, s.logger)

	authHandler := handlers.NewAuthHandler(authService, store, s.cfg)
	postHandler := handlers.NewPostHandler(s.postService, s.settingService, s.pageService, s.categoryService, s.tagService)
	categoryHandler := handlers.NewCategoryHandler(s.categoryService)
	tagHandler := handlers.NewTagHandler(s.tagService)
	commentHandler := handlers.NewCommentHandler(s.commentService)
	mediaHandler := handlers.NewMediaHandler(mediaService)
	searchHandler := handlers.NewSearchHandler(searchService)
	themeHandler := handlers.NewThemeHandler(s.themeService, s.reloadThemeViews)
	pluginHandler := handlers.NewPluginHandler(s.pluginService, s.logger)
	settingHandler := handlers.NewSettingHandler(s.settingService)
	dashboardHandler := handlers.NewDashboardHandler(s.db)
	navigationHandler := handlers.NewNavigationHandler(s.navigationService)
	pageHandler := handlers.NewPageHandler(s.pageService)

	api := s.app.Group("/api")

	// Rate limiting for public endpoints
	loginLimiter := limiter.New(limiter.Config{
		Max:        5,
		Expiration: 15 * time.Minute,
		KeyGenerator: func(c *fiber.Ctx) string {
			return c.IP()
		},
		LimitReached: func(c *fiber.Ctx) error {
			return c.Status(429).JSON(fiber.Map{"error": "登录尝试过于频繁，请15分钟后再试"})
		},
	})
	commentLimiter := limiter.New(limiter.Config{
		Max:        10,
		Expiration: 1 * time.Minute,
		KeyGenerator: func(c *fiber.Ctx) string {
			return c.IP()
		},
		LimitReached: func(c *fiber.Ctx) error {
			return c.Status(429).JSON(fiber.Map{"error": "操作过于频繁，请稍后再试"})
		},
	})
	searchLimiter := limiter.New(limiter.Config{
		Max:        30,
		Expiration: 1 * time.Minute,
		KeyGenerator: func(c *fiber.Ctx) string {
			return c.IP()
		},
		LimitReached: func(c *fiber.Ctx) error {
			return c.Status(429).JSON(fiber.Map{"error": "搜索请求过于频繁"})
		},
	})

	api.Post("/login", loginLimiter, authHandler.Login)
	api.Post("/logout", authHandler.Logout)
	api.Post("/admin/init", authHandler.InitAdmin)
	api.Get("/install/check", authHandler.CheckInstall)
	api.Post("/install/test-db", authHandler.TestDB)
	api.Post("/admin/init/setup", authHandler.SetupInstall)
	api.Put("/admin/init/settings", settingHandler.Update)
	api.Get("/csrf", func(c *fiber.Ctx) error {
		return c.JSON(fiber.Map{"csrf_token": c.Locals("csrf")})
	})

	api.Get("/posts", postHandler.List)
	api.Get("/posts/:id", postHandler.Get)
	api.Get("/posts/slug/:slug", postHandler.GetBySlug)
	api.Get("/categories", categoryHandler.List)
	api.Get("/categories/:id", categoryHandler.Get)
	api.Get("/tags", tagHandler.List)
	api.Get("/tags/:id", tagHandler.Get)
	api.Get("/comments", commentHandler.List)
	api.Post("/comments", commentLimiter, commentHandler.Create)
	api.Post("/comments/:id/like", commentLimiter, commentHandler.Like)
	api.Get("/search", searchLimiter, searchHandler.Search)
	api.Get("/settings", settingHandler.GetPublic)
	api.Get("/navigations", navigationHandler.PublicList)
	api.Get("/pages", pageHandler.GetNavPages)
	api.Get("/pages/:slug", pageHandler.GetBySlug)

	admin := api.Group("/admin")
	admin.Use(middleware.AuthRequired(authService, store))
	admin.Use(middleware.AdminOnly())

	admin.Get("/dashboard", dashboardHandler.Stats)

	admin.Get("/posts", postHandler.AdminList)
	admin.Post("/posts", postHandler.Create)
	admin.Put("/posts/:id", postHandler.Update)
	admin.Delete("/posts/:id", postHandler.Delete)
	admin.Post("/posts/:id/publish", postHandler.Publish)
	admin.Post("/posts/:id/schedule", postHandler.Schedule)
	admin.Get("/posts/:id/versions", postHandler.Versions)

	admin.Get("/categories", categoryHandler.AdminList)
	admin.Post("/categories", categoryHandler.Create)
	admin.Put("/categories/:id", categoryHandler.Update)
	admin.Delete("/categories/:id", categoryHandler.Delete)

	admin.Get("/tags", tagHandler.AdminList)
	admin.Post("/tags", tagHandler.Create)
	admin.Put("/tags/:id", tagHandler.Update)
	admin.Delete("/tags/:id", tagHandler.Delete)
	admin.Post("/tags/merge", tagHandler.Merge)

	admin.Get("/comments", commentHandler.AdminList)
	admin.Put("/comments/:id/approve", commentHandler.Approve)
	admin.Put("/comments/:id/spam", commentHandler.MarkSpam)
	admin.Delete("/comments/:id", commentHandler.Delete)
	admin.Post("/comments/batch", commentHandler.BatchAction)

	admin.Get("/media", mediaHandler.List)
	admin.Post("/media/upload", mediaHandler.Upload)
	admin.Delete("/media/:id", mediaHandler.Delete)

	admin.Get("/themes", themeHandler.List)
	admin.Post("/themes/upload", themeHandler.Upload)
	admin.Post("/themes/:id/activate", themeHandler.Activate)
	admin.Delete("/themes/:id", themeHandler.Delete)
	admin.Get("/themes/:id/preview", themeHandler.Preview)
	admin.Get("/themes/:id/config", themeHandler.GetConfig)
	admin.Put("/themes/:id/config", themeHandler.UpdateConfig)

	admin.Get("/plugins", pluginHandler.List)
	admin.Post("/plugins/upload", pluginHandler.Upload)
	admin.Post("/plugins/:id/activate", pluginHandler.Activate)
	admin.Post("/plugins/:id/deactivate", pluginHandler.Deactivate)
	admin.Delete("/plugins/:id", pluginHandler.Delete)
	admin.Get("/plugins/:id/config", pluginHandler.GetConfig)
	admin.Put("/plugins/:id/config", pluginHandler.UpdateConfig)
	admin.Post("/plugins/:id/approve", pluginHandler.ApprovePermissions)

	admin.Get("/settings", settingHandler.GetAll)
	admin.Put("/settings", settingHandler.Update)
	admin.Post("/settings/logo", mediaHandler.UploadLogo)
	admin.Post("/settings/favicon", mediaHandler.UploadFavicon)

	admin.Get("/friend-links", settingHandler.ListFriendLinks)
	admin.Post("/friend-links", settingHandler.CreateFriendLink)
	admin.Put("/friend-links/:id", settingHandler.UpdateFriendLink)
	admin.Delete("/friend-links/:id", settingHandler.DeleteFriendLink)

	admin.Get("/navigations", navigationHandler.List)
	admin.Post("/navigations", navigationHandler.Create)
	admin.Put("/navigations/:id", navigationHandler.Update)
	admin.Delete("/navigations/:id", navigationHandler.Delete)
	admin.Post("/navigations/order", navigationHandler.UpdateOrder)

	admin.Get("/pages", pageHandler.List)
	admin.Get("/pages/:id", pageHandler.Get)
	admin.Post("/pages", pageHandler.Create)
	admin.Put("/pages/:id", pageHandler.Update)
	admin.Delete("/pages/:id", pageHandler.Delete)
	admin.Post("/pages/:id/publish", pageHandler.Publish)

	s.app.Get("/", s.renderHome)
	s.app.Get("/post/:slug", s.renderPost)
	s.app.Get("/category/:slug", s.renderCategory)
	s.app.Get("/categories", s.renderCategories)
	s.app.Get("/tag/:slug", s.renderTag)
	s.app.Get("/tags", s.renderTags)
	s.app.Get("/archives", s.renderArchives)
	s.app.Get("/page/:slug", s.renderPage)
	s.app.Get("/search", s.renderSearch)
	s.app.Get("/rss", postHandler.RSS)
	s.app.Get("/sitemap.xml", postHandler.Sitemap)
	s.app.Get("/install", s.renderInstall)
	s.app.Get("/login", s.renderLogin)
	api.Get("/challenge/check", middleware.CheckChallenge(s.pluginService))
	api.Post("/challenge/verify/:type", middleware.VerifyChallenge(s.pluginService))
	s.app.Get("/admin", s.authPageMiddleware, s.renderAdmin)
	s.app.Get("/admin/*", s.authPageMiddleware, s.renderAdmin)

	s.app.Static("/uploads", s.cfg.Storage.Path)
	s.app.Static("/assets", "./web/assets")
	s.app.Static("/plugins", "./plugins")
}

func (s *Server) getSettings() map[string]string {
	settings, err := s.settingService.GetPublic()
	if err != nil {
		s.logger.Error("Failed to get settings", zap.Error(err))
		return make(map[string]string)
	}
	return settings
}

// reloadThemeViews creates a new template engine from the active theme's template directory
// and replaces the app's Views, enabling live theme switching without restart.
func (s *Server) reloadThemeViews() {
	templatePath := s.themeService.GetTemplatePath()
	engine := html.New(templatePath, ".html")
	engine.Reload(true)
	engine.Debug(true)
	registerTemplateFuncs(engine)
	s.themeViews = engine
}

// render renders a template using the active theme's engine.
func (s *Server) render(c *fiber.Ctx, name string, bind fiber.Map, layouts ...string) error {
	// Inject active theme config into template data
	if bind == nil {
		bind = fiber.Map{}
	}
	if _, ok := bind["themeConfig"]; !ok {
		theme, err := s.themeService.GetActive()
		if err == nil {
			resolved := s.themeService.GetResolvedConfig(theme)
			bind["themeConfig"] = resolved.Values
		}
	}

	// Inject active plugin head/body snippets
	if _, ok := bind["pluginHead"]; !ok {
		head, body := s.pluginService.GetActiveInjects()
		bind["pluginHead"] = template.HTML(head)
		bind["pluginBody"] = template.HTML(body)
	}

	buf := new(bytes.Buffer)
	if err := s.themeViews.Render(buf, name, bind, layouts...); err != nil {
		return err
	}
	c.Set("Content-Type", "text/html; charset=utf-8")
	return c.Send(buf.Bytes())
}

func registerTemplateFuncs(engine *html.Engine) {
	engine.AddFunc("safe", func(s string) template.HTML {
		return template.HTML(s)
	})
	engine.AddFunc("substr", func(s string, start, end int) string {
		if start < 0 {
			start = 0
		}
		if end > len(s) {
			end = len(s)
		}
		return s[start:end]
	})
	engine.AddFunc("truncate", func(length int, s string) string {
		if length >= len(s) {
			return s
		}
		return s[:length]
	})
	engine.AddFunc("add", func(a, b int) int { return a + b })
	engine.AddFunc("sub", func(a, b int) int { return a - b })
	engine.AddFunc("mul", func(a, b int) int { return a * b })
	engine.AddFunc("div", func(a, b int) int { if b == 0 { return 0 }; return a / b })
	engine.AddFunc("upper", strings.ToUpper)
	engine.AddFunc("lower", strings.ToLower)

	md := goldmark.New(
		goldmark.WithExtensions(
			extension.GFM,
			extension.Typographer,
		),
		goldmark.WithRendererOptions(
			gmhtml.WithHardWraps(),
			gmhtml.WithXHTML(),
		),
	)

	engine.AddFunc("markdown", func(content string) template.HTML {
		var buf bytes.Buffer
		if err := md.Convert([]byte(content), &buf); err != nil {
			return template.HTML(content)
		}
		return template.HTML(buf.String())
	})

	engine.AddFunc("stripMarkdown", func(content string) string {
		// Remove images first (before links, since ![alt](url) contains [alt])
		re := regexp.MustCompile(`!\[([^\]]*)\]\([^)]+\)`)
		content = re.ReplaceAllString(content, "")

		re = regexp.MustCompile(`(?m)^#{1,6}\s+`)
		content = re.ReplaceAllString(content, "")

		// Remove fenced code blocks (```...```)
		re = regexp.MustCompile("(?s)```[^`]*```")
		content = re.ReplaceAllString(content, "")

		// Remove inline code
		re = regexp.MustCompile("`([^`]+)`")
		content = re.ReplaceAllString(content, "$1")

		re = regexp.MustCompile(`\*\*(.+?)\*\*`)
		content = re.ReplaceAllString(content, "$1")

		re = regexp.MustCompile(`\*(.+?)\*`)
		content = re.ReplaceAllString(content, "$1")

		re = regexp.MustCompile(`__(.+?)__`)
		content = re.ReplaceAllString(content, "$1")

		re = regexp.MustCompile(`_(.+?)_`)
		content = re.ReplaceAllString(content, "$1")

		re = regexp.MustCompile(`~~(.+?)~~`)
		content = re.ReplaceAllString(content, "$1")

		re = regexp.MustCompile(`\[([^\]]+)\]\([^)]+\)`)
		content = re.ReplaceAllString(content, "$1")

		re = regexp.MustCompile(`(?m)^>\s+`)
		content = re.ReplaceAllString(content, "")

		re = regexp.MustCompile(`(?m)^[-*+]\s+`)
		content = re.ReplaceAllString(content, "")

		re = regexp.MustCompile(`(?m)^\d+\.\s+`)
		content = re.ReplaceAllString(content, "")

		content = strings.ReplaceAll(content, "\n", " ")
		content = strings.ReplaceAll(content, "\r", " ")

		spaceRe := regexp.MustCompile(`\s+`)
		content = spaceRe.ReplaceAllString(content, " ")

		return strings.TrimSpace(content)
	})
}

func (s *Server) getNavigations() []models.Navigation {
	navs, err := s.navigationService.GetActive()
	if err != nil {
		s.logger.Error("Failed to get navigations", zap.Error(err))
		return []models.Navigation{}
	}
	return navs
}

func (s *Server) getNavPages() []models.Page {
	pages, err := s.pageService.GetNavPages()
	if err != nil {
		s.logger.Error("Failed to get nav pages", zap.Error(err))
		return []models.Page{}
	}
	return pages
}

func (s *Server) renderHome(c *fiber.Ctx) error {
	posts, _, err := s.postService.List(services.PostListParams{
		Page:     1,
		PageSize: 10,
		Status:   models.PostStatusPublished,
	})
	if err != nil {
		s.logger.Error("Failed to get posts", zap.Error(err))
		return s.render(c, "home", fiber.Map{"posts": []models.Post{}})
	}

	settings := s.getSettings()

	return s.render(c, "home", fiber.Map{
		"posts":       posts,
		"title":       settings["site_title"],
		"settings":    settings,
		"navigations": s.getNavigations(),
		"currentPath": "/",
	}, "base")
}

func (s *Server) renderLogin(c *fiber.Ctx) error {
	settings := s.getSettings()
	buf := new(bytes.Buffer)
	if err := s.systemViews.Render(buf, "login", fiber.Map{"settings": settings}); err != nil {
		return err
	}
	c.Set("Content-Type", "text/html; charset=utf-8")
	return c.Send(buf.Bytes())
}

func (s *Server) renderInstall(c *fiber.Ctx) error {
	// If already installed, redirect to login
	var count int64
	s.db.Model(&models.User{}).Where("role = ?", "admin").Count(&count)
	if count > 0 {
		return c.Redirect("/login")
	}
	buf := new(bytes.Buffer)
	if err := s.systemViews.Render(buf, "install", fiber.Map{}); err != nil {
		return err
	}
	c.Set("Content-Type", "text/html; charset=utf-8")
	return c.Send(buf.Bytes())
}

func (s *Server) renderPost(c *fiber.Ctx) error {
	slug := c.Params("slug")
	
    post, err := s.postService.GetBySlug(slug)
    if err != nil {
        return s.render404(c)
    }

    // Apply shortcode transformations from active plugins
    post.Content = s.pluginService.ProcessShortcodes(post.Content)

    comments, err := s.commentService.GetNestedComments(post.ID)
    if err != nil {
        s.logger.Error("Failed to get comments", zap.Error(err))
        comments = []models.Comment{}
    }

    settings := s.getSettings()

    return s.render(c, "post", fiber.Map{
        "post":         post,
        "comments":     comments,
        "commentCount": len(comments),
        "pageTitle":   post.Title,
        "settings":     settings,
        "navigations": s.getNavigations(),
        "currentPath":  "/post/" + slug,
    }, "base")
}

func (s *Server) renderCategory(c *fiber.Ctx) error {
	slugOrID := c.Params("slug")
    
    var category *models.Category
    var err error
    
    category, err = s.categoryService.GetBySlug(slugOrID)
    if err != nil {
        var id uint
        if _, parseErr := fmt.Sscanf(slugOrID, "%d", &id); parseErr == nil {
            category, err = s.categoryService.GetByID(id)
        }
    }
    
    if err != nil {
        return s.render404(c)
    }

    posts, _, err := s.postService.List(services.PostListParams{
        Page:     1,
        PageSize: 20,
        Status:   models.PostStatusPublished,
        Category: category.ID,
    })
    if err != nil {
        s.logger.Error("Failed to get posts", zap.Error(err))
        posts = []models.Post{}
    }

    settings := s.getSettings()

    return s.render(c, "category", fiber.Map{
        "category":    category,
        "posts":       posts,
        "pageTitle":  category.Name,
        "settings":    settings,
        "navigations": s.getNavigations(),
        "currentPath":  "/category/" + slugOrID,
    }, "base")
}

func (s *Server) renderTag(c *fiber.Ctx) error {
	slugOrID := c.Params("slug")
    
    var tag *models.Tag
    var err error
    
    tag, err = s.tagService.GetBySlug(slugOrID)
    if err != nil {
        var id uint
        if _, parseErr := fmt.Sscanf(slugOrID, "%d", &id); parseErr == nil {
            tag, err = s.tagService.GetByID(id)
        }
    }
    
    if err != nil {
        return s.render404(c)
    }

    posts, _, err := s.postService.List(services.PostListParams{
        Page:     1,
        PageSize: 20,
        Status:   models.PostStatusPublished,
        Tag:      tag.ID,
    })
    if err != nil {
        s.logger.Error("Failed to get posts", zap.Error(err))
        posts = []models.Post{}
    }

    settings := s.getSettings()

    return s.render(c, "tag", fiber.Map{
        "tag":         tag,
        "posts":       posts,
        "pageTitle":   tag.Name,
        "settings":    settings,
        "navigations": s.getNavigations(),
        "currentPath":  "/tag/" + slugOrID,
    }, "base")
}

func (s *Server) renderCategories(c *fiber.Ctx) error {
	categories, err := s.categoryService.List()
	if err != nil {
		s.logger.Error("Failed to get categories", zap.Error(err))
		categories = []models.Category{}
	}

	settings := s.getSettings()

	return s.render(c, "categories", fiber.Map{
		"categories":  categories,
		"pageTitle":   "分类",
		"settings":    settings,
		"navigations": s.getNavigations(),
		"currentPath":  "/categories",
	}, "base")
}

func (s *Server) renderTags(c *fiber.Ctx) error {
	tags, err := s.tagService.List()
	if err != nil {
		s.logger.Error("Failed to get tags", zap.Error(err))
		tags = []models.Tag{}
	}

	settings := s.getSettings()

	return s.render(c, "tags", fiber.Map{
		"tags":        tags,
		"pageTitle":   "标签",
		"settings":    settings,
		"navigations": s.getNavigations(),
		"currentPath":  "/tags",
	}, "base")
}

func (s *Server) renderArchives(c *fiber.Ctx) error {
	posts, _, err := s.postService.List(services.PostListParams{
		Page:     1,
		PageSize: 100,
		Status:   models.PostStatusPublished,
	})
	if err != nil {
		s.logger.Error("Failed to get posts", zap.Error(err))
		posts = []models.Post{}
	}

	archives := make(map[string][]models.Post)
	for _, post := range posts {
		if post.PublishedAt != nil {
			month := post.PublishedAt.Format("2006-01")
			archives[month] = append(archives[month], post)
		}
	}

	settings := s.getSettings()

	return s.render(c, "archives", fiber.Map{
		"archives":    archives,
		"pageTitle":   "归档",
		"settings":    settings,
		"navigations": s.getNavigations(),
		"currentPath":  "/archives",
	}, "base")
}

func (s *Server) renderPage(c *fiber.Ctx) error {
	slug := c.Params("slug")

	page, err := s.pageService.GetBySlug(slug)
	if err != nil {
		return s.render404(c)
	}

	// Apply shortcode transformations from active plugins
	page.Content = s.pluginService.ProcessShortcodes(page.Content)

	s.pageService.IncrementViewCount(page.ID)

	settings := s.getSettings()

	return s.render(c, "page", fiber.Map{
		"page":        page,
		"pageTitle":   page.Title,
		"settings":    settings,
		"navigations": s.getNavigations(),
		"currentPath":  "/page/" + slug,
	}, "base")
}

func (s *Server) renderSearch(c *fiber.Ctx) error {
    settings := s.getSettings()
    return s.render(c, "search", fiber.Map{
        "query":       c.Query("q"),
        "pageTitle":  "搜索",
        "settings":    settings,
        "navigations": s.getNavigations(),
        "currentPath":  "/search",
    }, "base")
}

func (s *Server) render404(c *fiber.Ctx) error {
	settings := s.getSettings()
	c.Status(404)
	return s.render(c, "404", fiber.Map{
		"pageTitle":   "404",
		"settings":    settings,
		"navigations": s.getNavigations(),
	}, "base")
}

func (s *Server) renderAdmin(c *fiber.Ctx) error {
	settings := s.getSettings()
	buf := new(bytes.Buffer)
	if err := s.systemViews.Render(buf, "admin", fiber.Map{"settings": settings}); err != nil {
		return err
	}
	c.Set("Content-Type", "text/html; charset=utf-8")
	return c.Send(buf.Bytes())
}

func (s *Server) authPageMiddleware(c *fiber.Ctx) error {
	token := c.Cookies("auth_token")
	if token == "" {
		return c.Redirect("/login")
	}
	_, err := s.authService.ValidateToken(token)
	if err != nil {
		return c.Redirect("/login")
	}
	return c.Next()
}

func (s *Server) Start() error {
	s.setupMiddleware()
	s.setupRoutes()

	addr := fmt.Sprintf("%s:%d", s.cfg.Server.Host, s.cfg.Server.Port)

	s.logger.Info("Server started", zap.String("address", addr))
	return s.app.Listen(addr)
}

func (s *Server) Shutdown(ctx context.Context) error {
	s.logger.Info("Shutting down server...")
	return s.app.ShutdownWithTimeout(s.cfg.Server.ShutdownTimeout)
}
