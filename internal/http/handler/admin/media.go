package admin

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

type MediaAPIHandler struct {
	storagePath string
}

func NewMediaAPIHandler(uploadsDir string) *MediaAPIHandler {
	if uploadsDir == "" {
		uploadsDir = "./uploads"
	}
	return &MediaAPIHandler{storagePath: uploadsDir}
}

func (h *MediaAPIHandler) Upload(w http.ResponseWriter, r *http.Request) {
	if err := r.ParseMultipartForm(32 << 20); err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "file too large (max 32MB)"})
		return
	}

	file, header, err := r.FormFile("file")
	if err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "no file provided"})
		return
	}
	defer file.Close()

	ext := strings.ToLower(filepath.Ext(header.Filename))
	allowedExts := map[string]bool{
		".jpg": true, ".jpeg": true, ".png": true, ".gif": true, ".webp": true, ".svg": true,
		".ico": true, ".bmp": true, ".tiff": true, ".tif": true,
	}
	if !allowedExts[ext] {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": "unsupported file type"})
		return
	}

	dateDir := time.Now().Format("2006/01")
	dir := filepath.Join(h.storagePath, dateDir)
	if err := os.MkdirAll(dir, 0755); err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "failed to create directory"})
		return
	}

	filename := fmt.Sprintf("%d%s", time.Now().UnixNano(), ext)
	destPath := filepath.Join(dir, filename)

	dst, err := os.Create(destPath)
	if err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "failed to save file"})
		return
	}
	defer dst.Close()

	if _, err := io.Copy(dst, file); err != nil {
		writeJSON(w, http.StatusInternalServerError, map[string]string{"error": "failed to save file"})
		return
	}

	url := "/" + filepath.Join("uploads", dateDir, filename)
	writeJSON(w, http.StatusOK, map[string]string{
		"url":      url,
		"filename": filename,
	})
}

func (h *MediaAPIHandler) List(w http.ResponseWriter, r *http.Request) {
	type fileEntry struct {
		URL      string `json:"url"`
		Filename string `json:"filename"`
	}

	var files []fileEntry
	filepath.Walk(h.storagePath, func(path string, info os.FileInfo, err error) error {
		if err != nil || info == nil || info.IsDir() {
			return nil
		}
		rel, _ := filepath.Rel(h.storagePath, path)
		files = append(files, fileEntry{
			URL:      "/" + filepath.Join("uploads", rel),
			Filename: info.Name(),
		})
		return nil
	})

	if files == nil {
		files = []fileEntry{}
	}
	writeJSON(w, http.StatusOK, map[string]interface{}{
		"files": files,
	})
}
