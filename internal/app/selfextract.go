package app

import (
	"embed"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
)

type EmbeddedFS struct {
	AdminTemplates embed.FS
	AdminStatic    embed.FS
	DefaultTheme   embed.FS
	Configs        embed.FS
	Plugins        embed.FS
}

type ExtractedPaths struct {
	DataDir        string
	AdminTemplates string
	AdminStatic    string
	ThemesDir      string
	PluginsDir     string
	ConfigsDir     string
	DefaultConfig  string
}

func SelfExtract(dataDir string, emb *EmbeddedFS, logger *slog.Logger) (*ExtractedPaths, error) {
	p := &ExtractedPaths{
		DataDir:        dataDir,
		AdminTemplates: filepath.Join(dataDir, "web/admin/templates"),
		AdminStatic:    filepath.Join(dataDir, "web/admin/static"),
		ThemesDir:      filepath.Join(dataDir, "themes"),
		PluginsDir:     filepath.Join(dataDir, "plugins"),
		ConfigsDir:     filepath.Join(dataDir, "configs"),
		DefaultConfig:  filepath.Join(dataDir, "configs/default.yaml"),
	}

	marker := filepath.Join(dataDir, ".extracted")
	_, markerErr := os.Stat(marker)

	logger.Info("self-extract: ensuring embedded files are present", slog.String("dir", dataDir))

	extractions := []struct {
		src    embed.FS
		prefix string
		dst    string
	}{
		{emb.AdminTemplates, "web/admin/templates", p.AdminTemplates},
		{emb.AdminStatic, "web/admin/static", p.AdminStatic},
		{emb.DefaultTheme, "themes/default", filepath.Join(p.ThemesDir, "default")},
		{emb.Plugins, "plugins", p.PluginsDir},
		{emb.Configs, "configs", p.ConfigsDir},
	}

	for _, e := range extractions {
		sub, err := fs.Sub(e.src, e.prefix)
		if err != nil {
			return nil, fmt.Errorf("sub fs %s: %w", e.prefix, err)
		}
		if err := extractFS(sub, e.dst); err != nil {
			return nil, fmt.Errorf("extract to %s: %w", e.dst, err)
		}
	}

	if markerErr != nil {
		if err := os.WriteFile(marker, []byte("polaris"), 0644); err != nil {
			return nil, fmt.Errorf("write marker: %w", err)
		}
		logger.Info("self-extract: extraction complete")
	}
	return p, nil
}

func extractFS(src fs.FS, dst string) error {
	return fs.WalkDir(src, ".", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}

		target := filepath.Join(dst, path)

		if d.IsDir() {
			return os.MkdirAll(target, 0755)
		}

		if _, statErr := os.Stat(target); statErr == nil {
			return nil
		}

		data, readErr := fs.ReadFile(src, path)
		if readErr != nil {
			return readErr
		}

		if mkdirErr := os.MkdirAll(filepath.Dir(target), 0755); mkdirErr != nil {
			return mkdirErr
		}

		perm := fs.FileMode(0644)
		if strings.HasSuffix(path, ".wasm") {
			perm = 0755
		}

		return os.WriteFile(target, data, perm)
	})
}
