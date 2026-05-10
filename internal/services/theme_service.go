package services

import (
	"archive/zip"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/polaris/blog/internal/config"
	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type ThemeService struct {
	db     *gorm.DB
	cfg    *config.Config
	logger *zap.Logger
}

// safeExtractZip safely extracts a zip reader into destDir, stripping a top-level
// directory prefix if all entries share one. Returns an error for path traversal attempts.
func safeExtractZip(r *zip.ReadCloser, destDir string) error {
	// Detect top-level directory prefix for stripping
	prefix := ""
	for _, f := range r.File {
		if f.FileInfo().IsDir() && strings.Contains(f.Name, "/") {
			parts := strings.SplitN(f.Name, "/", 2)
			if parts[0] != "" && parts[0] != "__MACOSX" {
				prefix = parts[0] + "/"
				break
			}
		}
	}

	for _, f := range r.File {
		name := f.Name
		// Strip top-level directory prefix
		if prefix != "" && strings.HasPrefix(name, prefix) {
			name = strings.TrimPrefix(name, prefix)
		}
		if name == "" {
			continue
		}

		// Security: reject macOS metadata and hidden files
		base := filepath.Base(name)
		if strings.HasPrefix(base, ".") || strings.HasPrefix(base, "__") {
			continue
		}

		// Security: reject path traversal
		cleaned := filepath.Clean(name)
		if strings.HasPrefix(cleaned, "..") || strings.Contains(cleaned, string(os.PathSeparator)+".."+string(os.PathSeparator)) {
			return fmt.Errorf("安全错误: 检测到路径穿越尝试: %s", name)
		}

		fpath := filepath.Join(destDir, cleaned)

		if f.FileInfo().IsDir() {
			os.MkdirAll(fpath, 0755)
			continue
		}

		if err := os.MkdirAll(filepath.Dir(fpath), 0755); err != nil {
			return err
		}

		outFile, err := os.OpenFile(fpath, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0644)
		if err != nil {
			return err
		}

		rc, err := f.Open()
		if err != nil {
			outFile.Close()
			return err
		}

		_, err = io.Copy(outFile, rc)
		outFile.Close()
		rc.Close()

		if err != nil {
			return err
		}
	}
	return nil
}

// ThemeConfigField defines a single config option in theme.json
type ThemeConfigField struct {
	Label    string `json:"label"`               // Display name
	Type     string `json:"type"`                // "switch", "text", "textarea", "color", "select", "number"
	Default  string `json:"default"`             // Default value
	Options  string `json:"options,omitempty"`    // For "select" type: comma-separated values
	Group    string `json:"group,omitempty"`      // Optional group name for UI grouping
}

// ThemeConfig is the schema parsed from theme.json
type ThemeConfig struct {
	Name        string                       `json:"name"`
	Slug        string                       `json:"slug"`
	Version     string                       `json:"version"`
	Author      string                       `json:"author"`
	Description string                       `json:"description"`
	Config      map[string]string            `json:"config"`      // Default config values (kept for backward compat)
	ConfigDefs  map[string]ThemeConfigField   `json:"config_defs"` // Rich config definitions with types
}

// ResolvedThemeConfig is the final merged config passed to templates
type ResolvedThemeConfig struct {
	Fields   map[string]ThemeConfigField // Schema definitions
	Values   map[string]string          // Resolved values (user override or default)
}

func NewThemeService(db *gorm.DB, cfg *config.Config, logger *zap.Logger) *ThemeService {
	return &ThemeService{db: db, cfg: cfg, logger: logger}
}

func (s *ThemeService) List() ([]models.Theme, error) {
	var themes []models.Theme
	if err := s.db.Order("is_active DESC, name ASC").Find(&themes).Error; err != nil {
		return nil, err
	}
	return themes, nil
}

func (s *ThemeService) GetByID(id uint) (*models.Theme, error) {
	var theme models.Theme
	if err := s.db.First(&theme, id).Error; err != nil {
		return nil, err
	}
	return &theme, nil
}

func (s *ThemeService) GetActive() (*models.Theme, error) {
	var theme models.Theme
	if err := s.db.Where("is_active = ?", true).First(&theme).Error; err != nil {
		return nil, err
	}
	return &theme, nil
}

func (s *ThemeService) Upload(zipPath string) (*models.Theme, error) {
	r, err := zip.OpenReader(zipPath)
	if err != nil {
		return nil, errors.New("主题包格式不正确")
	}
	defer r.Close()

	var themeConfig *ThemeConfig
	var extractPath string

	for _, f := range r.File {
		if strings.HasSuffix(f.Name, "theme.json") || strings.HasSuffix(f.Name, "theme.yaml") {
			rc, err := f.Open()
			if err != nil {
				return nil, err
			}
			data, err := io.ReadAll(rc)
			rc.Close()
			if err != nil {
				return nil, err
			}

			themeConfig = &ThemeConfig{}
			if err := json.Unmarshal(data, themeConfig); err != nil {
				return nil, errors.New("主题配置文件格式错误")
			}
		}
	}

	if themeConfig == nil {
		return nil, errors.New("主题包缺少配置文件")
	}

	var existing models.Theme
	if err := s.db.Where("slug = ?", themeConfig.Slug).First(&existing).Error; err == nil {
		return nil, errors.New("主题已存在")
	}

	themesDir := filepath.Join("themes")
	if err := os.MkdirAll(themesDir, 0755); err != nil {
		return nil, err
	}

	extractPath = filepath.Join(themesDir, themeConfig.Slug)
	if err := os.MkdirAll(extractPath, 0755); err != nil {
		return nil, err
	}

	if err := safeExtractZip(r, extractPath); err != nil {
		os.RemoveAll(extractPath)
		return nil, err
	}

	configJSON, _ := json.Marshal(themeConfig.Config)

	theme := &models.Theme{
		Name:        themeConfig.Name,
		Slug:        themeConfig.Slug,
		Version:     themeConfig.Version,
		Author:      themeConfig.Author,
		Description: themeConfig.Description,
		Path:        extractPath,
		IsActive:    false,
		Config:      string(configJSON),
	}

	if err := s.db.Create(theme).Error; err != nil {
		os.RemoveAll(extractPath)
		return nil, err
	}

	return theme, nil
}

func (s *ThemeService) Activate(id uint) error {
	var theme models.Theme
	if err := s.db.First(&theme, id).Error; err != nil {
		return err
	}

	s.db.Model(&models.Theme{}).Where("is_active = ?", true).Update("is_active", false)

	return s.db.Model(&theme).Update("is_active", true).Error
}

func (s *ThemeService) Delete(id uint) error {
	var theme models.Theme
	if err := s.db.First(&theme, id).Error; err != nil {
		return err
	}

	if theme.IsActive {
		return errors.New("无法删除正在使用的主题")
	}

	if theme.Slug == DefaultThemeSlug {
		return errors.New("无法删除内置默认主题")
	}

	os.RemoveAll(theme.Path)

	return s.db.Delete(&theme).Error
}

func (s *ThemeService) Preview(id uint) (string, error) {
	theme, err := s.GetByID(id)
	if err != nil {
		return "", err
	}

	return theme.Path, nil
}

func (s *ThemeService) UpdateConfig(id uint, config map[string]string) error {
	configJSON, err := json.Marshal(config)
	if err != nil {
		return err
	}

	return s.db.Model(&models.Theme{}).Where("id = ?", id).Update("config", string(configJSON)).Error
}

// GetConfig returns the user-saved config values for a theme
func (s *ThemeService) GetConfig(id uint) (map[string]string, error) {
	theme, err := s.GetByID(id)
	if err != nil {
		return nil, err
	}
	if theme.Config == "" {
		return make(map[string]string), nil
	}
	var config map[string]string
	if err := json.Unmarshal([]byte(theme.Config), &config); err != nil {
		return make(map[string]string), nil
	}
	return config, nil
}

// GetConfigDefs returns the config definitions (schema) from the theme's theme.json
func (s *ThemeService) GetConfigDefs(theme *models.Theme) map[string]ThemeConfigField {
	themeJSONPath := filepath.Join(theme.Path, "theme.json")
	data, err := os.ReadFile(themeJSONPath)
	if err != nil {
		return nil
	}
	var tc ThemeConfig
	if err := json.Unmarshal(data, &tc); err != nil {
		return nil
	}
	return tc.ConfigDefs
}

// GetResolvedConfig merges user-saved config with defaults from theme.json
func (s *ThemeService) GetResolvedConfig(theme *models.Theme) *ResolvedThemeConfig {
	result := &ResolvedThemeConfig{
		Fields: make(map[string]ThemeConfigField),
		Values: make(map[string]string),
	}

	// Load schema definitions from theme.json
	configDefs := s.GetConfigDefs(theme)
	for key, field := range configDefs {
		result.Fields[key] = field
		result.Values[key] = field.Default // Start with defaults
	}

	// Also load legacy flat config defaults
	if configDefs == nil || len(configDefs) == 0 {
		themeJSONPath := filepath.Join(theme.Path, "theme.json")
		data, err := os.ReadFile(themeJSONPath)
		if err == nil {
			var tc ThemeConfig
			if json.Unmarshal(data, &tc) == nil {
				for key, val := range tc.Config {
					result.Values[key] = val
				}
			}
		}
	}

	// Override with user-saved values from DB
	if theme.Config != "" {
		var userConfig map[string]string
		if json.Unmarshal([]byte(theme.Config), &userConfig) == nil {
			for key, val := range userConfig {
				result.Values[key] = val
			}
		}
	}

	return result
}

const (
	DefaultThemeSlug = "default"
	DefaultThemePath = "themes/default"
)

func (s *ThemeService) GetTemplatePath() string {
	theme, err := s.GetActive()
	if err != nil {
		return DefaultThemePath + "/templates"
	}
	// Verify the theme directory actually exists
	templateDir := filepath.Join(theme.Path, "templates")
	if info, err := os.Stat(templateDir); err != nil || !info.IsDir() {
		s.logger.Warn("Active theme template directory not found, falling back to default",
			zap.String("path", templateDir))
		return DefaultThemePath + "/templates"
	}
	return templateDir
}

func (s *ThemeService) EnsureDefaultTheme() {
	var existing models.Theme
	err := s.db.Where("slug = ?", DefaultThemeSlug).First(&existing).Error
	if err == nil {
		// Default theme already exists in DB, ensure path is correct
		s.db.Model(&existing).Update("path", DefaultThemePath)
		return
	}

	theme := &models.Theme{
		Name:     "Default",
		Slug:     DefaultThemeSlug,
		Version:  "1.0.0",
		Author:   "Polaris",
		Path:     DefaultThemePath,
		IsActive: true,
	}

	s.db.Create(theme)
	s.logger.Info("Default theme initialized")
}
