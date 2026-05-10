package embed

import (
	"embed"
	"io/fs"
	"os"
	"path/filepath"

	"go.uber.org/zap"
)

//go:embed all:default_theme
var defaultThemeFS embed.FS

//go:embed all:system_templates
var systemTemplatesFS embed.FS

// ReleaseDefaultTheme extracts the embedded default theme to targetDir/themes/default/.
// Existing files are preserved (not overwritten).
func ReleaseDefaultTheme(targetDir string, logger *zap.Logger) error {
	dest := filepath.Join(targetDir, "themes", "default")
	return releaseFS(defaultThemeFS, "default_theme", dest, logger)
}

// ReleaseSystemTemplates extracts the embedded system templates to targetDir/web/templates/.
// Existing files are NOT preserved — system templates are always overwritten to stay in sync.
func ReleaseSystemTemplates(targetDir string, logger *zap.Logger) error {
	dest := filepath.Join(targetDir, "web", "templates")
	return releaseFS(systemTemplatesFS, "system_templates", dest, logger)
}

// releaseFS copies files from an embed.FS sub-directory to dest on disk.
func releaseFS(embedFS fs.FS, embedPrefix, dest string, logger *zap.Logger) error {
	subFS, err := fs.Sub(embedFS, embedPrefix)
	if err != nil {
		return err
	}

	return fs.WalkDir(subFS, ".", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}

		target := filepath.Join(dest, path)
		if d.IsDir() {
			return os.MkdirAll(target, 0755)
		}

		// For system templates, always overwrite; for themes, only write if not exists
		if embedPrefix == "default_theme" {
			if _, err := os.Stat(target); err == nil {
				return nil // file exists, skip
			}
		}

		data, err := fs.ReadFile(subFS, path)
		if err != nil {
			return err
		}

		if err := os.MkdirAll(filepath.Dir(target), 0755); err != nil {
			return err
		}

		return os.WriteFile(target, data, 0644)
	})
}
