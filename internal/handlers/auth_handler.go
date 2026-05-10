package handlers

import (
	"fmt"
	"time"

	"github.com/gofiber/fiber/v2"
	"github.com/gofiber/fiber/v2/middleware/session"
	"github.com/polaris/blog/internal/config"
	"github.com/polaris/blog/internal/database"
	"github.com/polaris/blog/internal/models"
	"github.com/polaris/blog/internal/services"
)

type AuthHandler struct {
	authService *services.AuthService
	store       *session.Store
	cfg         *config.Config
}

func NewAuthHandler(authService *services.AuthService, store *session.Store, cfg *config.Config) *AuthHandler {
	return &AuthHandler{
		authService: authService,
		store:       store,
		cfg:         cfg,
	}
}

type LoginRequest struct {
	Username string `json:"username"`
	Password string `json:"password"`
	Remember bool   `json:"remember"`
}

type LoginResponse struct {
	Token string      `json:"token"`
	User  *models.User `json:"user"`
}

func (h *AuthHandler) Login(c *fiber.Ctx) error {
	var req LoginRequest
	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if req.Username == "" || req.Password == "" {
		return c.Status(400).JSON(fiber.Map{"error": "用户名和密码不能为空"})
	}

	ip := c.IP()
	token, user, err := h.authService.Login(req.Username, req.Password, ip)
	if err != nil {
		return c.Status(401).JSON(fiber.Map{"error": err.Error()})
	}

	expiry := h.cfg.Auth.TokenExpiry
	if req.Remember {
		expiry = 30 * 24 * time.Hour
	}

	c.Cookie(&fiber.Cookie{
		Name:     "auth_token",
		Value:    token,
		Expires:  time.Now().Add(expiry),
		HTTPOnly: true,
		Secure:   true,
		SameSite: "Lax",
	})

	return c.JSON(LoginResponse{Token: token, User: user})
}

func (h *AuthHandler) Logout(c *fiber.Ctx) error {
	c.Cookie(&fiber.Cookie{
		Name:     "auth_token",
		Value:    "",
		Expires:  time.Now().Add(-time.Hour),
		HTTPOnly: true,
	})

	return c.JSON(fiber.Map{"message": "已退出登录"})
}

func (h *AuthHandler) GetCurrentUser(c *fiber.Ctx) error {
	user := c.Locals("user").(*models.User)
	return c.JSON(user)
}

func (h *AuthHandler) ChangePassword(c *fiber.Ctx) error {
	userID := c.Locals("userID").(uint)

	var req struct {
		OldPassword string `json:"old_password"`
		NewPassword string `json:"new_password"`
	}

	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if err := h.authService.ChangePassword(userID, req.OldPassword, req.NewPassword); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"message": "密码修改成功"})
}

func (h *AuthHandler) InitAdmin(c *fiber.Ctx) error {
	// Check if admin already exists
	var count int64
	if err := h.authService.DB().Model(&models.User{}).Where("role = ?", models.UserRoleAdmin).Count(&count).Error; err == nil && count > 0 {
		return c.Status(409).JSON(fiber.Map{"error": "管理员账户已存在", "exists": true})
	}

	var req struct {
		Username string `json:"username"`
		Email    string `json:"email"`
		Password string `json:"password"`
	}

	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if len(req.Username) < 3 || len(req.Username) > 50 {
		return c.Status(400).JSON(fiber.Map{"error": "用户名长度需在3-50个字符之间"})
	}

	if len(req.Password) < 8 {
		return c.Status(400).JSON(fiber.Map{"error": "密码长度不能少于8个字符"})
	}

	if err := h.authService.CreateAdmin(req.Username, req.Email, req.Password); err != nil {
		return c.Status(409).JSON(fiber.Map{"error": err.Error()})
	}

	return c.JSON(fiber.Map{"message": "管理员创建成功"})
}

// CheckInstall returns whether the system has been initialized (admin exists).
func (h *AuthHandler) CheckInstall(c *fiber.Ctx) error {
	var count int64
	h.authService.DB().Model(&models.User{}).Where("role = ?", models.UserRoleAdmin).Count(&count)
	return c.JSON(fiber.Map{"installed": count > 0})
}

// TestDB tests a database connection using the provided config.
func (h *AuthHandler) TestDB(c *fiber.Ctx) error {
	var req struct {
		Driver   string `json:"driver"`
		Host     string `json:"host"`
		Port     int    `json:"port"`
		User     string `json:"user"`
		Password string `json:"password"`
		DBName   string `json:"dbname"`
	}
	if err := c.BodyParser(&req); err != nil {
		return c.JSON(fiber.Map{"ok": false, "message": "无效的请求数据"})
	}

	// Build a temporary config for connection test
	testCfg := &config.Config{
		Database: config.DatabaseConfig{
			Driver:   req.Driver,
			Host:     req.Host,
			Port:     req.Port,
			User:     req.User,
			Password: req.Password,
			DBName:   req.DBName,
		},
	}

	db, err := database.Connect(testCfg)
	if err != nil {
		return c.JSON(fiber.Map{"ok": false, "message": fmt.Sprintf("连接失败: %v", err)})
	}

	sqlDB, _ := db.DB()
	if sqlDB != nil {
		sqlDB.Close()
	}

	driverLabel := "SQLite"
	if req.Driver == "mysql" {
		driverLabel = "MySQL"
	} else if req.Driver == "postgres" {
		driverLabel = "PostgreSQL"
	}
	return c.JSON(fiber.Map{"ok": true, "message": fmt.Sprintf("%s 连接成功", driverLabel)})
}

// SetupInstall handles the full installation: write config, init DB, create admin, save settings.
func (h *AuthHandler) SetupInstall(c *fiber.Ctx) error {
	// Double-check not already installed
	var count int64
	h.authService.DB().Model(&models.User{}).Where("role = ?", models.UserRoleAdmin).Count(&count)
	if count > 0 {
		return c.Status(409).JSON(fiber.Map{"error": "系统已安装，请勿重复安装", "exists": true})
	}

	var req struct {
		Database config.DatabaseConfig `json:"database"`
		Settings map[string]string     `json:"settings"`
		Admin    struct {
			Username string `json:"username"`
			Email    string `json:"email"`
			Password string `json:"password"`
		} `json:"admin"`
	}

	if err := c.BodyParser(&req); err != nil {
		return c.Status(400).JSON(fiber.Map{"error": "无效的请求数据"})
	}

	if len(req.Admin.Username) < 3 || len(req.Admin.Username) > 50 {
		return c.Status(400).JSON(fiber.Map{"error": "用户名长度需在3-50个字符之间"})
	}
	if len(req.Admin.Password) < 8 {
		return c.Status(400).JSON(fiber.Map{"error": "密码长度不能少于8个字符"})
	}

	// 1. Write config.yaml
	if err := config.SaveInstallConfig(&req.Database); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "写入配置文件失败: " + err.Error()})
	}

	// 2. Test database connection with new config
	testCfg := &config.Config{
		Database: config.DatabaseConfig{
			Driver:   req.Database.Driver,
			Host:     req.Database.Host,
			Port:     req.Database.Port,
			User:     req.Database.User,
			Password: req.Database.Password,
			DBName:   req.Database.DBName,
		},
	}
	newDB, err := database.Connect(testCfg)
	if err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "数据库连接失败: " + err.Error()})
	}

	// 3. Run migrations
	if err := database.Migrate(newDB); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "数据库迁移失败: " + err.Error()})
	}

	// 4. Create admin using new DB connection
	authSvc := services.NewAuthService(newDB, h.cfg, nil)
	if err := authSvc.CreateAdmin(req.Admin.Username, req.Admin.Email, req.Admin.Password); err != nil {
		return c.Status(500).JSON(fiber.Map{"error": "创建管理员失败: " + err.Error()})
	}

	// 5. Save site settings
	settingSvc := services.NewSettingService(newDB, nil)
	for key, value := range req.Settings {
		if value != "" {
			settingSvc.Set(key, value)
		}
	}

	// Close test connection
	if sqlDB, _ := newDB.DB(); sqlDB != nil {
		sqlDB.Close()
	}

	return c.JSON(fiber.Map{"message": "安装成功"})
}
