package middleware

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/gofiber/fiber/v2"
	"github.com/polaris/blog/internal/services"
)

const challengeCookie = "polaris_challenge"
const challengeExpiry = 24 * time.Hour

// ChallengeMiddleware is a generic hook: if any active plugin with
// "challenge_type" is active, the middleware passes through and lets
// the plugin's inject_body handle the challenge UI inline.
// The middleware only checks cookies; the plugin handles the rest.
func ChallengeMiddleware(pluginService *services.PluginService) fiber.Handler {
	return func(c *fiber.Ctx) error {
		// Skip non-page routes
		path := c.Path()
		if strings.HasPrefix(path, "/api/") || strings.HasPrefix(path, "/admin") ||
			strings.HasPrefix(path, "/login") || strings.HasPrefix(path, "/install") ||
			strings.HasPrefix(path, "/uploads") || strings.HasPrefix(path, "/assets") ||
			path == "/rss" || path == "/sitemap.xml" {
			return c.Next()
		}

		cfg := getChallengeConfig(pluginService)
		if cfg == nil || cfg.Type == "" {
			return c.Next()
		}

		// If cookie present and valid, pass through (plugin overlay won't show)
		cookie := c.Cookies(challengeCookie + "_" + cfg.Type)
		if cookie != "" && validateCookie(cookie, cfg.Secret) {
			return c.Next()
		}

		// No valid cookie — pass through. The plugin's inject_body overlay
		// will detect the missing cookie and show the challenge widget.
		return c.Next()
	}
}

// CheckChallenge handles GET /api/challenge/check — returns whether the
// client already has a valid challenge cookie (so the JS overlay can skip).
func CheckChallenge(pluginService *services.PluginService) fiber.Handler {
	return func(c *fiber.Ctx) error {
		cfg := getChallengeConfig(pluginService)
		if cfg == nil || cfg.Type == "" {
			return c.JSON(fiber.Map{"verified": true})
		}
		cookie := c.Cookies(challengeCookie + "_" + cfg.Type)
		if cookie != "" && validateCookie(cookie, cfg.Secret) {
			return c.JSON(fiber.Map{"verified": true})
		}
		return c.JSON(fiber.Map{"verified": false})
	}
}

// VerifyChallenge handles POST /api/challenge/verify
func VerifyChallenge(pluginService *services.PluginService) fiber.Handler {
	return func(c *fiber.Ctx) error {
		challengeType := c.Params("type")
		if challengeType == "" {
			return c.Status(400).JSON(fiber.Map{"success": false, "error": "缺少类型"})
		}

		var req struct {
			Token    string `json:"token"`
			Redirect string `json:"redirect"`
		}
		if err := c.BodyParser(&req); err != nil || req.Token == "" {
			return c.Status(400).JSON(fiber.Map{"success": false, "error": "缺少验证令牌"})
		}

		cfg := getChallengeConfig(pluginService)
		if cfg == nil || cfg.Type != challengeType {
			return c.Status(400).JSON(fiber.Map{"success": false, "error": "未启用验证"})
		}

		var ok bool
		var err error

		switch cfg.Type {
		case "turnstile":
			ok, err = verifyTurnstile(req.Token, cfg.Secret, c.IP())
		default:
			return c.Status(400).JSON(fiber.Map{"success": false, "error": "不支持的验证类型"})
		}

		if err != nil {
			return c.Status(500).JSON(fiber.Map{"success": false, "error": "验证服务异常"})
		}
		if !ok {
			return c.Status(403).JSON(fiber.Map{"success": false, "error": "验证失败，请重试"})
		}

		// Set verified cookie
		value := generateCookie(cfg.Secret)
		c.Cookie(&fiber.Cookie{
			Name:     challengeCookie + "_" + cfg.Type,
			Value:    value,
			Path:     "/",
			MaxAge:   int(challengeExpiry.Seconds()),
			HTTPOnly: true,
			Secure:   true,
			SameSite: "Lax",
		})

		redirect := req.Redirect
		if redirect == "" || !strings.HasPrefix(redirect, "/") {
			redirect = "/"
		}

		return c.JSON(fiber.Map{"success": true, "redirect": redirect})
	}
}

type challengeConfig struct {
	Type    string // "turnstile", "recaptcha", etc.
	SiteKey string
	Secret  string
	Enabled bool
}

func getChallengeConfig(pluginService *services.PluginService) *challengeConfig {
	plugins, err := pluginService.GetActivePlugins()
	if err != nil {
		return nil
	}

	for _, plugin := range plugins {
		resolved := pluginService.GetResolvedConfig(&plugin)
		if resolved == nil {
			continue
		}

		challengeType := resolved.Values["challenge_type"]
		enabled := resolved.Values["enabled"] == "true"
		if challengeType == "" || !enabled {
			continue
		}

		siteKey := resolved.Values["site_key"]
		secret := resolved.Values["secret_key"]
		if siteKey == "" || secret == "" {
			continue
		}

		return &challengeConfig{
			Type:    challengeType,
			SiteKey: siteKey,
			Secret:  secret,
			Enabled: enabled,
		}
	}

	return nil
}

func verifyTurnstile(token, secret, ip string) (bool, error) {
	body := "secret=" + secret + "&response=" + token
	if ip != "" {
		body += "&remoteip=" + ip
	}

	resp, err := http.Post(
		"https://challenges.cloudflare.com/turnstile/v0/siteverify",
		"application/x-www-form-urlencoded",
		strings.NewReader(body),
	)
	if err != nil {
		return false, err
	}
	defer resp.Body.Close()

	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return false, err
	}

	var result struct {
		Success bool `json:"success"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return false, err
	}
	return result.Success, nil
}

func generateCookie(secret string) string {
	mac := hmac.New(sha256.New, []byte(secret))
	expiry := time.Now().Add(challengeExpiry).Unix()
	mac.Write([]byte(fmt.Sprintf("%d", expiry)))
	return fmt.Sprintf("%d.%s", expiry, hex.EncodeToString(mac.Sum(nil)))
}

func validateCookie(value, secret string) bool {
	parts := strings.SplitN(value, ".", 2)
	if len(parts) != 2 {
		return false
	}

	mac := hmac.New(sha256.New, []byte(secret))
	mac.Write([]byte(parts[0]))
	expected := hex.EncodeToString(mac.Sum(nil))
	if !hmac.Equal([]byte(parts[1]), []byte(expected)) {
		return false
	}

	var expiry int64
	fmt.Sscanf(parts[0], "%d", &expiry)
	return time.Now().Unix() < expiry
}
