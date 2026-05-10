package services

import (
	"archive/zip"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"text/template"

	"github.com/polaris/blog/internal/config"
	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"gorm.io/gorm"
)

type PluginService struct {
	db     *gorm.DB
	cfg    *config.Config
	logger *zap.Logger
	hooks  map[string][]func(interface{}) error
}

type PluginConfig struct {
	Name        string            `json:"name"`
	Slug        string            `json:"slug"`
	Version     string            `json:"version"`
	Author      string            `json:"author"`
	Description string            `json:"description"`
	Permissions []string          `json:"permissions"`
	Config      map[string]string `json:"config"`
	ConfigDefs  map[string]PluginConfigField `json:"config_defs"`
	InjectHead  string            `json:"inject_head,omitempty"`  // HTML to inject in <head>
	InjectBody  string            `json:"inject_body,omitempty"`  // HTML to inject before </body>
	Shortcodes  map[string]string `json:"shortcodes,omitempty"`  // shortcode name → Go template for ::name{key="val"} replacement
}

type PluginConfigField struct {
	Label    string `json:"label"`
	Type     string `json:"type"`                 // "switch", "text", "textarea", "color", "select", "number"
	Default  string `json:"default"`
	Options  string `json:"options,omitempty"`     // For "select" type
	Group    string `json:"group,omitempty"`
}

type ResolvedPluginConfig struct {
	Fields map[string]PluginConfigField
	Values map[string]string
}

type PluginPermission struct {
	Name        string `json:"name"`
	Description string `json:"description"`
}

var defaultPermissions = map[string]PluginPermission{
	"api_access":   {"API 访问", "可以调用系统 API"},
	"database":     {"数据库访问", "可以读写数据库"},
	"file_system":  {"文件系统", "可以读写文件系统"},
	"network":      {"网络访问", "可以发起网络请求"},
	"admin_ui":     {"后台界面", "可以在后台添加管理页面"},
}

func NewPluginService(db *gorm.DB, cfg *config.Config, logger *zap.Logger) *PluginService {
	return &PluginService{
		db:     db,
		cfg:    cfg,
		logger: logger,
		hooks:  make(map[string][]func(interface{}) error),
	}
}

func (s *PluginService) List() ([]models.Plugin, error) {
	var plugins []models.Plugin
	if err := s.db.Order("is_active DESC, name ASC").Find(&plugins).Error; err != nil {
		return nil, err
	}
	return plugins, nil
}

func (s *PluginService) GetByID(id uint) (*models.Plugin, error) {
	var plugin models.Plugin
	if err := s.db.First(&plugin, id).Error; err != nil {
		return nil, err
	}
	return &plugin, nil
}

func (s *PluginService) GetBySlug(slug string) (*models.Plugin, error) {
	var plugin models.Plugin
	if err := s.db.Where("slug = ?", slug).First(&plugin).Error; err != nil {
		return nil, err
	}
	return &plugin, nil
}

func (s *PluginService) Upload(zipPath string) (*models.Plugin, error) {
	r, err := zip.OpenReader(zipPath)
	if err != nil {
		return nil, errors.New("插件包格式不正确")
	}
	defer r.Close()

	var pluginConfig *PluginConfig

	for _, f := range r.File {
		if strings.HasSuffix(f.Name, "plugin.json") {
			rc, err := f.Open()
			if err != nil {
				return nil, err
			}
			data, err := io.ReadAll(rc)
			rc.Close()
			if err != nil {
				return nil, err
			}

			pluginConfig = &PluginConfig{}
			if err := json.Unmarshal(data, pluginConfig); err != nil {
				return nil, errors.New("插件配置文件格式错误")
			}
		}
	}

	if pluginConfig == nil {
		return nil, errors.New("插件包缺少配置文件")
	}

	if len(pluginConfig.Permissions) == 0 {
		return nil, errors.New("插件权限声明不完整")
	}

	pluginsDir := filepath.Join("plugins")
	if err := os.MkdirAll(pluginsDir, 0755); err != nil {
		return nil, err
	}

	extractPath := filepath.Join(pluginsDir, pluginConfig.Slug)
	if err := os.MkdirAll(extractPath, 0755); err != nil {
		return nil, err
	}

	if err := safeExtractZip(r, extractPath); err != nil {
		os.RemoveAll(extractPath)
		return nil, err
	}

	permissionsJSON, _ := json.Marshal(pluginConfig.Permissions)
	configJSON, _ := json.Marshal(pluginConfig.Config)

	var existing models.Plugin
	if err := s.db.Unscoped().Where("slug = ?", pluginConfig.Slug).First(&existing).Error; err == nil {
		if existing.DeletedAt.Valid {
			// Soft-deleted record exists — restore it
			s.db.Unscoped().Model(&existing).Updates(map[string]interface{}{
				"name":        pluginConfig.Name,
				"version":     pluginConfig.Version,
				"author":      pluginConfig.Author,
				"description": pluginConfig.Description,
				"path":        extractPath,
				"is_active":   false,
				"permissions": string(permissionsJSON),
				"config":      string(configJSON),
				"deleted_at":  nil,
			})
			return &existing, nil
		}
		os.RemoveAll(extractPath)
		return nil, fmt.Errorf("插件 %s 已存在，如需更新请先删除旧版本", pluginConfig.Name)
	}

	plugin := &models.Plugin{
		Name:        pluginConfig.Name,
		Slug:        pluginConfig.Slug,
		Version:     pluginConfig.Version,
		Author:      pluginConfig.Author,
		Description: pluginConfig.Description,
		Path:        extractPath,
		IsActive:    false,
		Permissions: string(permissionsJSON),
		Config:      string(configJSON),
	}

	if err := s.db.Create(plugin).Error; err != nil {
		os.RemoveAll(extractPath)
		return nil, err
	}

	return plugin, nil
}

func (s *PluginService) Activate(id uint) error {
	var plugin models.Plugin
	if err := s.db.First(&plugin, id).Error; err != nil {
		return err
	}

	return s.db.Model(&plugin).Update("is_active", true).Error
}

func (s *PluginService) Deactivate(id uint) error {
	var plugin models.Plugin
	if err := s.db.First(&plugin, id).Error; err != nil {
		return err
	}

	return s.db.Model(&plugin).Update("is_active", false).Error
}

func (s *PluginService) Delete(id uint) error {
	var plugin models.Plugin
	if err := s.db.First(&plugin, id).Error; err != nil {
		return err
	}

	if plugin.IsActive {
		return errors.New("请先禁用插件后再删除")
	}

	os.RemoveAll(plugin.Path)

	return s.db.Delete(&plugin).Error
}

func (s *PluginService) ApprovePermissions(id uint) error {
	var plugin models.Plugin
	if err := s.db.First(&plugin, id).Error; err != nil {
		return err
	}

	return s.Activate(id)
}

func (s *PluginService) GetPermissions() map[string]PluginPermission {
	return defaultPermissions
}

func (s *PluginService) ValidatePermissions(requested []string) error {
	for _, perm := range requested {
		if _, ok := defaultPermissions[perm]; !ok {
			return errors.New("未知的权限: " + perm)
		}
	}
	return nil
}

func (s *PluginService) RegisterHook(event string, handler func(interface{}) error) {
	s.hooks[event] = append(s.hooks[event], handler)
}

func (s *PluginService) ExecuteHook(event string, data interface{}) error {
	handlers, ok := s.hooks[event]
	if !ok {
		return nil
	}

	for _, handler := range handlers {
		if err := handler(data); err != nil {
			s.logger.Error("Hook execution failed", 
				zap.String("event", event), 
				zap.Error(err))
			return err
		}
	}
	return nil
}

func (s *PluginService) UpdateConfig(id uint, config map[string]string) error {
	configJSON, err := json.Marshal(config)
	if err != nil {
		return err
	}

	return s.db.Model(&models.Plugin{}).Where("id = ?", id).Update("config", string(configJSON)).Error
}

func (s *PluginService) GetConfig(id uint) (map[string]string, error) {
	plugin, err := s.GetByID(id)
	if err != nil {
		return nil, err
	}
	if plugin.Config == "" {
		return make(map[string]string), nil
	}
	var config map[string]string
	if err := json.Unmarshal([]byte(plugin.Config), &config); err != nil {
		return make(map[string]string), nil
	}
	return config, nil
}

func (s *PluginService) GetConfigDefs(plugin *models.Plugin) map[string]PluginConfigField {
	pluginJSONPath := filepath.Join(plugin.Path, "plugin.json")
	data, err := os.ReadFile(pluginJSONPath)
	if err != nil {
		return nil
	}
	var pc PluginConfig
	if err := json.Unmarshal(data, &pc); err != nil {
		return nil
	}
	return pc.ConfigDefs
}

func (s *PluginService) GetResolvedConfig(plugin *models.Plugin) *ResolvedPluginConfig {
	result := &ResolvedPluginConfig{
		Fields: make(map[string]PluginConfigField),
		Values: make(map[string]string),
	}

	configDefs := s.GetConfigDefs(plugin)
	for key, field := range configDefs {
		result.Fields[key] = field
		result.Values[key] = field.Default
	}

	if configDefs == nil || len(configDefs) == 0 {
		pluginJSONPath := filepath.Join(plugin.Path, "plugin.json")
		data, err := os.ReadFile(pluginJSONPath)
		if err == nil {
			var pc PluginConfig
			if json.Unmarshal(data, &pc) == nil {
				for key, val := range pc.Config {
					result.Values[key] = val
				}
			}
		}
	}

	if plugin.Config != "" {
		var userConfig map[string]string
		if json.Unmarshal([]byte(plugin.Config), &userConfig) == nil {
			for key, val := range userConfig {
				result.Values[key] = val
			}
		}
	}

	return result
}

// GetActiveInjects returns head/body HTML snippets from all active plugins.
// Plugin config values are substituted into {{KEY}} placeholders.
func (s *PluginService) GetActiveInjects() (headHTML, bodyHTML string) {
	plugins, err := s.GetActivePlugins()
	if err != nil {
		return
	}
	for _, plugin := range plugins {
		pluginJSONPath := filepath.Join(plugin.Path, "plugin.json")
		data, err := os.ReadFile(pluginJSONPath)
		if err != nil {
			continue
		}
		var pc PluginConfig
		if json.Unmarshal(data, &pc) != nil {
			continue
		}
		// Build replacement map from resolved config
		replacements := make(map[string]string)
		resolved := s.GetResolvedConfig(&plugin)
		if resolved != nil {
			for k, v := range resolved.Values {
				replacements[strings.ToUpper(k)] = v
			}
		}
		if pc.InjectHead != "" {
			headHTML += replacePlaceholders(pc.InjectHead, replacements) + "\n"
		}
		if pc.InjectBody != "" {
			bodyHTML += replacePlaceholders(pc.InjectBody, replacements) + "\n"
		}
	}
	return
}

// replacePlaceholders replaces {{KEY}} patterns in html with values from the map.
func replacePlaceholders(html string, values map[string]string) string {
	for key, val := range values {
		html = strings.ReplaceAll(html, "{{"+key+"}}", val)
	}
	return html
}

func (s *PluginService) GetActivePlugins() ([]models.Plugin, error) {
	var plugins []models.Plugin
	if err := s.db.Where("is_active = ?", true).Find(&plugins).Error; err != nil {
		return nil, err
	}
	return plugins, nil
}

// shortcodeRe matches ::type{key="value", key2="value2"} patterns.
var shortcodeRe = regexp.MustCompile(`::(\w+)\{([^}]+)\}`)

// ProcessShortcodes scans content for ::name{key="val"} patterns, looks up
// matching shortcode definitions from all active plugins, and replaces them
// with the rendered Go template. This is a generic mechanism — the core does
// not hardcode any specific shortcode logic.
func (s *PluginService) ProcessShortcodes(content string) string {
	plugins, err := s.GetActivePlugins()
	if err != nil {
		return content
	}

	// Build a map of shortcode name → Go template from all active plugins.
	shortcodes := make(map[string]string)
	for _, p := range plugins {
		pc := s.loadPluginConfig(&p)
		if pc == nil {
			continue
		}
		for name, tmpl := range pc.Shortcodes {
			if tmpl != "" {
				shortcodes[name] = tmpl
			}
		}
	}
	if len(shortcodes) == 0 {
		return content
	}

	return shortcodeRe.ReplaceAllStringFunc(content, func(match string) string {
		sub := shortcodeRe.FindStringSubmatch(match)
		if len(sub) != 3 {
			return match
		}
		tmpl, ok := shortcodes[sub[1]]
		if !ok {
			return match
		}

		// Parse key="value" pairs.
		params := make(map[string]string)
		for _, pair := range strings.Split(sub[2], ",") {
			pair = strings.TrimSpace(pair)
			eq := strings.Index(pair, "=")
			if eq < 0 {
				continue
			}
			key := strings.TrimSpace(pair[:eq])
			val := strings.TrimSpace(pair[eq+1:])
			val = strings.Trim(val, "\"'")
			params[key] = val
		}

		t, err := template.New("sc").Parse(tmpl)
		if err != nil {
			s.logger.Warn("Invalid shortcode template",
				zap.String("shortcode", sub[1]),
				zap.Error(err))
			return match
		}
		var buf bytes.Buffer
		if err := t.Execute(&buf, params); err != nil {
			return match
		}
		return buf.String()
	})
}

// loadPluginConfig reads the plugin.json from a plugin's directory.
func (s *PluginService) loadPluginConfig(plugin *models.Plugin) *PluginConfig {
	path := filepath.Join(plugin.Path, "plugin.json")
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var pc PluginConfig
	if err := json.Unmarshal(data, &pc); err != nil {
		return nil
	}
	return &pc
}
