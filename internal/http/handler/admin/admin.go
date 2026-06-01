package admin

import (
	"context"
	"encoding/json"
	"fmt"
	"html/template"
	"io/fs"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"github.com/go-chi/chi/v5"
	"github.com/polaris-blog/blog/internal/http/middleware"
	"github.com/polaris-blog/blog/internal/i18n"
	"github.com/polaris-blog/blog/internal/repository"
	"github.com/polaris-blog/blog/internal/service"
	"github.com/polaris-blog/blog/internal/theme"
)

type AdminHandler struct {
	postService     *service.PostService
	commentService  *service.CommentService
	authService     *service.AuthService
	categoryService *service.CategoryService
	tagService      *service.TagService
	themeManager    *theme.Manager
	options         repository.OptionRepository
	templates       *template.Template
	i18nBundle      *i18n.Bundle
}

func NewAdminHandler(
	postService *service.PostService,
	commentService *service.CommentService,
	authService *service.AuthService,
	categoryService *service.CategoryService,
	tagService *service.TagService,
	themeManager *theme.Manager,
	options repository.OptionRepository,
	templateDir string,
	i18nBundle *i18n.Bundle,
) *AdminHandler {
	h := &AdminHandler{
		postService:     postService,
		commentService:  commentService,
		authService:     authService,
		categoryService: categoryService,
		tagService:      tagService,
		themeManager:    themeManager,
		options:         options,
		i18nBundle:      i18nBundle,
	}
	h.templates = h.loadTemplates(templateDir)
	return h
}

func (h *AdminHandler) loadTemplates(dir string) *template.Template {
	tmpl := template.New("").Funcs(template.FuncMap{
		"date_format":   func(t interface{}, format string) string { return fmt.Sprintf("%v", t) },
		"parse_browser": func(ua string) string { return theme.ParseBrowser(ua) },
		"parse_os":      func(ua string) string { return theme.ParseOS(ua) },
		"t":             func(key string) string { return key },
		"status_text":   func(s string) string { return s },
		"json": func(v interface{}) template.JS {
			b, err := json.Marshal(v)
			if err != nil {
				return template.JS("{}")
			}
			safe := strings.ReplaceAll(string(b), "</script>", `<\/script>`)
			safe = strings.ReplaceAll(safe, "<!--", `<\!--`)
			return template.JS(safe)
		},
	})

	filepath.Walk(dir, func(path string, info fs.FileInfo, err error) error {
		if err != nil || info == nil || info.IsDir() || !strings.HasSuffix(path, ".html") {
			return nil
		}
		rel, relErr := filepath.Rel(dir, path)
		if relErr != nil {
			return nil
		}
		data, readErr := os.ReadFile(path)
		if readErr != nil {
			return nil
		}
		_, parseErr := tmpl.New(rel).Parse(string(data))
		if parseErr != nil {
			return nil
		}
		return nil
	})

	return tmpl
}

func (h *AdminHandler) render(w http.ResponseWriter, r *http.Request, name string, data map[string]interface{}) {
	if data == nil {
		data = make(map[string]interface{})
	}
	lang := "en"
	if h.options != nil {
		if title, err := h.options.Get(context.Background(), "site_title"); err == nil && title != "" {
			data["site_title"] = title
		} else {
			data["site_title"] = "Polaris"
		}
		if icon, err := h.options.Get(context.Background(), "site_icon"); err == nil && icon != "" {
			data["site_icon"] = icon
		}
		if siteLang, err := h.options.Get(context.Background(), "site_language"); err == nil && siteLang != "" {
			lang = siteLang
		}
	} else {
		data["site_title"] = "Polaris"
	}
	if cookie, err := r.Cookie("polaris_lang"); err == nil && cookie.Value != "" {
		lang = cookie.Value
	}
	data["lang"] = lang

	tFunc := func(key string) string {
		return h.i18nBundle.T(lang, key)
	}
	statusText := func(s string) string {
		key := "admin.status." + s
		translated := h.i18nBundle.T(lang, key)
		if translated == key {
			return s
		}
		return translated
	}
	if pt, ok := data["page_title"]; ok {
		if ptStr, ok := pt.(string); ok {
			i18nKey := "admin.page_title." + strings.ToLower(strings.ReplaceAll(ptStr, " ", "_"))
			translated := h.i18nBundle.T(lang, i18nKey)
			if translated != i18nKey {
				data["page_title"] = translated
			}
		}
	}
	tmpl := h.templates.Funcs(template.FuncMap{"t": tFunc, "status_text": statusText})

	if h.i18nBundle != nil {
		if i18nJSON, err := h.i18nBundle.ToJSON(lang); err == nil {
			data["i18n_json"] = template.JS(string(i18nJSON))
		}
	}

	if err := tmpl.ExecuteTemplate(w, name, data); err != nil {
		log.Printf("template error %s: %v", name, err)
	}
}

func navData(active string) map[string]string {
	return map[string]string{
		"nav_dashboard":  eq(active, "dashboard"),
		"nav_posts":      eq(active, "posts"),
		"nav_pages":      eq(active, "pages"),
		"nav_navigation": eq(active, "navigation"),
		"nav_categories": eq(active, "categories"),
		"nav_tags":       eq(active, "tags"),
		"nav_comments":   eq(active, "comments"),
		"nav_media":      eq(active, "media"),
		"nav_themes":     eq(active, "themes"),
		"nav_plugins":    eq(active, "plugins"),
		"nav_settings":   eq(active, "settings"),
	}
}

func eq(a, b string) string {
	if a == b {
		return "active"
	}
	return ""
}

func (h *AdminHandler) LoginPage(w http.ResponseWriter, r *http.Request) {
	h.render(w, r, "login.html", nil)
}

func (h *AdminHandler) Dashboard(w http.ResponseWriter, r *http.Request) {
	published, _ := h.postService.CountByStatus(r.Context(), "published")
	drafts, _ := h.postService.CountByStatus(r.Context(), "draft")
	recentPosts, _, _ := h.postService.List(r.Context(), 1, 5, "")

	pendingComments := int64(0)
	if h.commentService != nil {
		pendingComments, _ = h.commentService.CountByStatus(r.Context(), "pending")
	}

	data := map[string]interface{}{
		"page_title": "Dashboard",
		"stats": map[string]int64{
			"published_posts":  published,
			"draft_posts":      drafts,
			"pending_comments": pendingComments,
			"total_media":      0,
		},
		"recent_posts": recentPosts,
	}
	for k, v := range navData("dashboard") {
		data[k] = v
	}
	h.render(w, r, "dashboard.html", data)
}

func (h *AdminHandler) PostsList(w http.ResponseWriter, r *http.Request) {
	posts, _, _ := h.postService.ListByType(r.Context(), "post", 1, 50, "")
	data := map[string]interface{}{"page_title": "Posts", "posts": posts}
	for k, v := range navData("posts") {
		data[k] = v
	}
	h.render(w, r, "posts/list.html", data)
}

func (h *AdminHandler) PostNew(w http.ResponseWriter, r *http.Request) {
	categories, _ := h.categoryService.List(r.Context())
	tags, _ := h.tagService.List(r.Context())
	data := map[string]interface{}{"page_title": "New Post", "is_new": true, "categories": categories, "tags": tags}
	for k, v := range navData("posts") {
		data[k] = v
	}
	h.render(w, r, "posts/form.html", data)
}

func (h *AdminHandler) PostEdit(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	post, err := h.postService.GetByID(r.Context(), id)
	if err != nil {
		http.Error(w, "Post not found", http.StatusNotFound)
		return
	}
	categories, _ := h.categoryService.List(r.Context())
	tags, _ := h.tagService.List(r.Context())
	data := map[string]interface{}{"page_title": "Edit Post", "is_new": false, "post": post, "categories": categories, "tags": tags}
	for k, v := range navData("posts") {
		data[k] = v
	}
	h.render(w, r, "posts/form.html", data)
}

func (h *AdminHandler) CategoriesList(w http.ResponseWriter, r *http.Request) {
	categories, _ := h.categoryService.List(r.Context())
	data := map[string]interface{}{"page_title": "Categories", "categories": categories}
	for k, v := range navData("categories") {
		data[k] = v
	}
	h.render(w, r, "categories/list.html", data)
}

func (h *AdminHandler) TagsList(w http.ResponseWriter, r *http.Request) {
	tags, _ := h.tagService.List(r.Context())
	data := map[string]interface{}{"page_title": "Tags", "tags": tags}
	for k, v := range navData("tags") {
		data[k] = v
	}
	h.render(w, r, "tags/list.html", data)
}

func (h *AdminHandler) CommentsList(w http.ResponseWriter, r *http.Request) {
	filter := r.URL.Query().Get("filter")
	comments, _, _ := h.commentService.List(r.Context(), filter, 1, 100)
	data := map[string]interface{}{"page_title": "Comments", "comments": comments, "filter": filter}
	for k, v := range navData("comments") {
		data[k] = v
	}
	h.render(w, r, "comments/list.html", data)
}

func (h *AdminHandler) MediaList(w http.ResponseWriter, r *http.Request) {
	data := map[string]interface{}{"page_title": "Media"}
	for k, v := range navData("media") {
		data[k] = v
	}
	h.render(w, r, "media/list.html", data)
}

func (h *AdminHandler) ThemesList(w http.ResponseWriter, r *http.Request) {
	themes := h.themeManager.ListThemes()
	activeTheme := ""
	if t := h.themeManager.GetActiveTheme(); t != nil {
		activeTheme = t.Meta.ID
	}
	data := map[string]interface{}{"page_title": "Themes", "themes": themes, "active_theme": activeTheme}
	for k, v := range navData("themes") {
		data[k] = v
	}
	h.render(w, r, "themes/list.html", data)
}

func (h *AdminHandler) SettingsPage(w http.ResponseWriter, r *http.Request) {
	settings := map[string]string{
		"site_title":       "Polaris Blog",
		"site_description": "A dynamic blog powered by Polaris",
		"site_url":         "http://localhost:8080",
		"site_language":    "en",
	}
	for _, key := range []string{"site_title", "site_description", "site_url", "site_icon", "site_language"} {
		if val, err := h.options.Get(r.Context(), key); err == nil && val != "" {
			settings[key] = val
		}
	}
	data := map[string]interface{}{
		"page_title": "Settings",
		"settings":   settings,
	}
	for k, v := range navData("settings") {
		data[k] = v
	}
	h.render(w, r, "settings.html", data)
}

func (h *AdminHandler) NavigationPage(w http.ResponseWriter, r *http.Request) {
	data := map[string]interface{}{
		"page_title": "Navigation",
	}
	for k, v := range navData("navigation") {
		data[k] = v
	}
	h.render(w, r, "navigation.html", data)
}

func (h *AdminHandler) UpdateSettings(w http.ResponseWriter, r *http.Request) {
	var settings map[string]string
	if err := json.NewDecoder(r.Body).Decode(&settings); err != nil {
		http.Error(w, "invalid request", http.StatusBadRequest)
		return
	}
	allowedKeys := map[string]bool{
		"site_title": true, "site_description": true, "site_url": true,
		"site_icon": true, "site_language": true,
	}
	for k, v := range settings {
		if !allowedKeys[k] {
			continue
		}
		if err := h.options.Set(r.Context(), k, v); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}

func (h *AdminHandler) GetNavItems(w http.ResponseWriter, r *http.Request) {
	navJSON, _ := h.options.Get(r.Context(), "nav_items")
	if navJSON == "" || navJSON == "[]" || navJSON == "null" {
		defaults := []struct {
			Label string `json:"label"`
			URL   string `json:"url"`
		}{
			{Label: "Home", URL: "/"},
			{Label: "Archives", URL: "/archives"},
			{Label: "Categories", URL: "/categories"},
			{Label: "Tags", URL: "/tags"},
		}
		b, _ := json.Marshal(defaults)
		navJSON = string(b)
	}
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]json.RawMessage{"items": json.RawMessage(navJSON)})
}

func (h *AdminHandler) UpdateNavItems(w http.ResponseWriter, r *http.Request) {
	var payload struct {
		Items []struct {
			Label string `json:"label"`
			URL   string `json:"url"`
		} `json:"items"`
	}
	if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
		http.Error(w, "invalid request", http.StatusBadRequest)
		return
	}
	b, _ := json.Marshal(payload.Items)
	if err := h.options.Set(r.Context(), "nav_items", string(b)); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}

func (h *AdminHandler) CreatePostPage(w http.ResponseWriter, r *http.Request) {
	var input service.CreatePostInput
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}
	input.AuthorID = middleware.GetUserID(r)
	post, err := h.postService.Create(r.Context(), input)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusCreated)
	json.NewEncoder(w).Encode(post)
}

func (h *AdminHandler) CreatePage(w http.ResponseWriter, r *http.Request) {
	var input service.CreatePostInput
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}
	input.Type = "page"
	input.AuthorID = middleware.GetUserID(r)
	post, err := h.postService.Create(r.Context(), input)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusCreated)
	json.NewEncoder(w).Encode(post)
}

func (h *AdminHandler) UpdatePage(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	var input service.UpdatePostInput
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}
	pageType := "page"
	input.Type = &pageType
	post, err := h.postService.Update(r.Context(), id, input)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	json.NewEncoder(w).Encode(post)
}

func (h *AdminHandler) PagesList(w http.ResponseWriter, r *http.Request) {
	pages, _, _ := h.postService.ListByType(r.Context(), "page", 1, 100, "")
	data := map[string]interface{}{"page_title": "Pages", "pages": pages}
	for k, v := range navData("pages") {
		data[k] = v
	}
	h.render(w, r, "pages/list.html", data)
}

func (h *AdminHandler) PageNew(w http.ResponseWriter, r *http.Request) {
	data := map[string]interface{}{"page_title": "New Page", "is_new": true}
	for k, v := range navData("pages") {
		data[k] = v
	}
	h.render(w, r, "pages/form.html", data)
}

func (h *AdminHandler) PluginsList(w http.ResponseWriter, r *http.Request) {
	data := map[string]interface{}{"page_title": "Plugins"}
	for k, v := range navData("plugins") {
		data[k] = v
	}
	h.render(w, r, "plugins/list.html", data)
}

func (h *AdminHandler) PageEdit(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	page, err := h.postService.GetByID(r.Context(), id)
	if err != nil {
		http.Error(w, "Page not found", http.StatusNotFound)
		return
	}
	data := map[string]interface{}{"page_title": "Edit Page", "is_new": false, "post": page}
	for k, v := range navData("pages") {
		data[k] = v
	}
	h.render(w, r, "pages/form.html", data)
}
