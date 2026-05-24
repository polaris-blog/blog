package admin

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"github.com/go-chi/chi/v5"
	"github.com/polaris-blog/blog/internal/plugin"
	"github.com/polaris-blog/blog/internal/service"
	"github.com/polaris-blog/blog/internal/theme"
)

type CommentAPIHandler struct {
	commentService *service.CommentService
}

func NewCommentAPIHandler(commentService *service.CommentService) *CommentAPIHandler {
	return &CommentAPIHandler{commentService: commentService}
}

func (h *CommentAPIHandler) List(w http.ResponseWriter, r *http.Request) {
	status := r.URL.Query().Get("status")
	comments, total, err := h.commentService.List(r.Context(), status, 1, 100)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, map[string]interface{}{"items": comments, "total": total})
}

func (h *CommentAPIHandler) Approve(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := h.commentService.Approve(r.Context(), id); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "approved"})
}

func (h *CommentAPIHandler) Spam(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := h.commentService.MarkSpam(r.Context(), id); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "spam"})
}

func (h *CommentAPIHandler) Delete(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := h.commentService.Delete(r.Context(), id); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "deleted"})
}

type PluginAPIHandler struct {
	pluginManager *plugin.Manager
}

func NewPluginAPIHandler(pluginManager *plugin.Manager) *PluginAPIHandler {
	return &PluginAPIHandler{pluginManager: pluginManager}
}

func (h *PluginAPIHandler) List(w http.ResponseWriter, r *http.Request) {
	plugins := h.pluginManager.List()
	writeJSON(w, http.StatusOK, plugins)
}

func (h *PluginAPIHandler) Upload(w http.ResponseWriter, r *http.Request) {
	r.Body = http.MaxBytesReader(w, r.Body, 16<<20)

	file, header, err := r.FormFile("plugin")
	if err != nil {
		http.Error(w, "no file uploaded", http.StatusBadRequest)
		return
	}
	defer file.Close()

	if filepath.Ext(header.Filename) != ".wasm" {
		http.Error(w, "only .wasm files are allowed", http.StatusBadRequest)
		return
	}

	wasmBytes, err := io.ReadAll(file)
	if err != nil {
		http.Error(w, "read file failed", http.StatusInternalServerError)
		return
	}

	pluginID := strings.TrimSuffix(header.Filename, ".wasm")
	if pluginID == "" {
		http.Error(w, "invalid plugin filename", http.StatusBadRequest)
		return
	}

	if err := h.pluginManager.LoadWasm(r.Context(), pluginID, wasmBytes); err != nil {
		http.Error(w, fmt.Sprintf("load plugin failed: %s", err.Error()), http.StatusBadRequest)
		return
	}

	p, ok := h.pluginManager.Get(pluginID)
	if !ok {
		writeJSON(w, http.StatusCreated, map[string]interface{}{
			"status": "loaded",
			"plugin": map[string]string{"id": pluginID},
		})
		return
	}
	meta := p.Meta()
	writeJSON(w, http.StatusCreated, map[string]interface{}{
		"status": "loaded",
		"plugin": meta,
	})
}

func (h *PluginAPIHandler) Delete(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := h.pluginManager.UnloadWasm(r.Context(), id); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "unloaded"})
}

func (h *PluginAPIHandler) GetSettings(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	p, ok := h.pluginManager.Get(id)
	if !ok {
		http.Error(w, "plugin not found", http.StatusNotFound)
		return
	}
	settings, _ := h.pluginManager.GetPluginSettings(id)
	if settings == nil {
		settings = make(map[string]interface{})
	}
	writeJSON(w, http.StatusOK, map[string]interface{}{
		"meta":     p.Meta(),
		"settings": settings,
	})
}

func (h *PluginAPIHandler) UpdateSettings(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if _, ok := h.pluginManager.Get(id); !ok {
		http.Error(w, "plugin not found", http.StatusNotFound)
		return
	}

	var settings map[string]interface{}
	if err := json.NewDecoder(r.Body).Decode(&settings); err != nil {
		http.Error(w, "invalid JSON", http.StatusBadRequest)
		return
	}

	if err := h.pluginManager.UpdatePluginSettings(id, settings); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	writeJSON(w, http.StatusOK, map[string]string{"status": "saved"})
}

type CategoryAPIHandler struct {
	categoryService *service.CategoryService
}

func NewCategoryAPIHandler(categoryService *service.CategoryService) *CategoryAPIHandler {
	return &CategoryAPIHandler{categoryService: categoryService}
}

func (h *CategoryAPIHandler) Create(w http.ResponseWriter, r *http.Request) {
	var input service.CreateCategoryInput
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		http.Error(w, "invalid request", http.StatusBadRequest)
		return
	}
	category, err := h.categoryService.Create(r.Context(), input)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusCreated, category)
}

func (h *CategoryAPIHandler) Delete(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := h.categoryService.Delete(r.Context(), id); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "deleted"})
}

type TagAPIHandler struct {
	tagService *service.TagService
}

func NewTagAPIHandler(tagService *service.TagService) *TagAPIHandler {
	return &TagAPIHandler{tagService: tagService}
}

func (h *TagAPIHandler) Create(w http.ResponseWriter, r *http.Request) {
	var input service.CreateTagInput
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		http.Error(w, "invalid request", http.StatusBadRequest)
		return
	}
	tag, err := h.tagService.Create(r.Context(), input)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusCreated, tag)
}

func (h *TagAPIHandler) Delete(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := h.tagService.Delete(r.Context(), id); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "deleted"})
}

type ThemeAPIHandler struct {
	themeManager *theme.Manager
}

func NewThemeAPIHandler(themeManager *theme.Manager) *ThemeAPIHandler {
	return &ThemeAPIHandler{themeManager: themeManager}
}

func (h *ThemeAPIHandler) Activate(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := h.themeManager.SetActiveTheme(id); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "activated", "theme": id})
}

func (h *ThemeAPIHandler) GetSettings(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	t, ok := h.themeManager.GetTheme(id)
	if !ok {
		http.Error(w, "theme not found", http.StatusNotFound)
		return
	}
	writeJSON(w, http.StatusOK, map[string]interface{}{
		"meta":     t.Meta,
		"settings": t.Settings,
	})
}

func (h *ThemeAPIHandler) UpdateSettings(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	var settings map[string]interface{}
	if err := json.NewDecoder(r.Body).Decode(&settings); err != nil {
		http.Error(w, "invalid request", http.StatusBadRequest)
		return
	}
	if err := h.themeManager.UpdateThemeSettings(id, settings); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func (h *ThemeAPIHandler) Upload(w http.ResponseWriter, r *http.Request) {
	r.Body = http.MaxBytesReader(w, r.Body, 32<<20)

	file, _, err := r.FormFile("theme")
	if err != nil {
		http.Error(w, "no file uploaded", http.StatusBadRequest)
		return
	}
	defer file.Close()

	tmpDir, err := os.MkdirTemp("", "theme-upload-*")
	if err != nil {
		http.Error(w, "server error", http.StatusInternalServerError)
		return
	}
	defer os.RemoveAll(tmpDir)

	tmpPath := filepath.Join(tmpDir, "theme.zip")
	out, err := os.Create(tmpPath)
	if err != nil {
		http.Error(w, "server error", http.StatusInternalServerError)
		return
	}
	if _, err := io.Copy(out, file); err != nil {
		out.Close()
		http.Error(w, "save failed", http.StatusInternalServerError)
		return
	}
	out.Close()

	t, err := h.themeManager.InstallFromZip(tmpPath)
	if err != nil {
		http.Error(w, fmt.Sprintf("install failed: %s", err.Error()), http.StatusBadRequest)
		return
	}

	writeJSON(w, http.StatusCreated, map[string]interface{}{
		"status": "installed",
		"theme":  t.Meta,
	})
}

func (h *ThemeAPIHandler) Delete(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := h.themeManager.DeleteTheme(id); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "deleted"})
}
