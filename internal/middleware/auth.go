package middleware

import (
	"strings"

	"github.com/gofiber/fiber/v2"
	"github.com/gofiber/fiber/v2/middleware/session"
	"github.com/polaris/blog/internal/services"
)

func AuthRequired(authService *services.AuthService, store *session.Store) fiber.Handler {
	return func(c *fiber.Ctx) error {
		authHeader := c.Get("Authorization")
		if authHeader == "" {
			return c.Status(401).JSON(fiber.Map{"error": "未授权访问"})
		}

		tokenString := strings.TrimPrefix(authHeader, "Bearer ")
		if tokenString == authHeader {
			return c.Status(401).JSON(fiber.Map{"error": "无效的授权格式"})
		}

		claims, err := authService.ValidateToken(tokenString)
		if err != nil {
			return c.Status(401).JSON(fiber.Map{"error": "无效的令牌"})
		}

		user, err := authService.GetUserByID(claims.UserID)
		if err != nil {
			return c.Status(401).JSON(fiber.Map{"error": "用户不存在"})
		}

		if !user.IsActive {
			return c.Status(403).JSON(fiber.Map{"error": "账户已被禁用"})
		}

		c.Locals("user", user)
		c.Locals("userID", user.ID)
		c.Locals("userRole", user.Role)

		return c.Next()
	}
}

func AdminOnly() fiber.Handler {
	return func(c *fiber.Ctx) error {
		role, ok := c.Locals("userRole").(string)
		if !ok || role != "admin" {
			return c.Status(403).JSON(fiber.Map{"error": "需要管理员权限"})
		}
		return c.Next()
	}
}

func OptionalAuth(authService *services.AuthService) fiber.Handler {
	return func(c *fiber.Ctx) error {
		authHeader := c.Get("Authorization")
		if authHeader == "" {
			return c.Next()
		}

		tokenString := strings.TrimPrefix(authHeader, "Bearer ")
		if tokenString == authHeader {
			return c.Next()
		}

		claims, err := authService.ValidateToken(tokenString)
		if err != nil {
			return c.Next()
		}

		user, err := authService.GetUserByID(claims.UserID)
		if err != nil {
			return c.Next()
		}

		c.Locals("user", user)
		c.Locals("userID", user.ID)

		return c.Next()
	}
}
