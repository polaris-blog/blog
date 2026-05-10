package services

import (
	"errors"
	"time"

	"github.com/golang-jwt/jwt/v5"
	"github.com/polaris/blog/internal/config"
	"github.com/polaris/blog/internal/models"
	"go.uber.org/zap"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type AuthService struct {
	db     *gorm.DB
	cfg    *config.Config
	logger *zap.Logger
}

type Claims struct {
	UserID   uint   `json:"user_id"`
	Username string `json:"username"`
	Role     string `json:"role"`
	jwt.RegisteredClaims
}

func NewAuthService(db *gorm.DB, cfg *config.Config, logger *zap.Logger) *AuthService {
	return &AuthService{db: db, cfg: cfg, logger: logger}
}

// DB exposes the underlying GORM DB for admin existence checks.
func (s *AuthService) DB() *gorm.DB {
	return s.db
}

func (s *AuthService) Login(username, password, ip string) (string, *models.User, error) {
	var attempts int64
	cutoff := time.Now().Add(-s.cfg.Auth.LockoutDuration)
	s.db.Model(&models.LoginAttempt{}).
		Where("ip = ? AND success = ? AND created_at > ?", ip, false, cutoff).
		Count(&attempts)

	if attempts >= int64(s.cfg.Auth.MaxLoginAttempts) {
		return "", nil, errors.New("账户已锁定，请稍后重试")
	}

	var user models.User
	if err := s.db.Where("username = ? OR email = ?", username, username).First(&user).Error; err != nil {
		s.recordAttempt(ip, username, false)
		return "", nil, errors.New("用户名或密码错误")
	}

	if !user.IsActive {
		return "", nil, errors.New("账户已被禁用")
	}

	if err := bcrypt.CompareHashAndPassword([]byte(user.PasswordHash), []byte(password)); err != nil {
		s.recordAttempt(ip, username, false)
		return "", nil, errors.New("用户名或密码错误")
	}

	s.recordAttempt(ip, username, true)

	token, err := s.GenerateToken(&user)
	if err != nil {
		return "", nil, err
	}

	return token, &user, nil
}

func (s *AuthService) recordAttempt(ip, username string, success bool) {
	attempt := models.LoginAttempt{
		IP:       ip,
		Username: username,
		Success:  success,
	}
	s.db.Create(&attempt)
}

func (s *AuthService) GenerateToken(user *models.User) (string, error) {
	claims := &Claims{
		UserID:   user.ID,
		Username: user.Username,
		Role:     user.Role,
		RegisteredClaims: jwt.RegisteredClaims{
			ExpiresAt: jwt.NewNumericDate(time.Now().Add(s.cfg.Auth.TokenExpiry)),
			IssuedAt:  jwt.NewNumericDate(time.Now()),
			Issuer:    "polaris",
		},
	}

	token := jwt.NewWithClaims(jwt.SigningMethodHS256, claims)
	return token.SignedString([]byte(s.cfg.Auth.JWTSecret))
}

func (s *AuthService) ValidateToken(tokenString string) (*Claims, error) {
	token, err := jwt.ParseWithClaims(tokenString, &Claims{}, func(token *jwt.Token) (interface{}, error) {
		return []byte(s.cfg.Auth.JWTSecret), nil
	})

	if err != nil {
		return nil, err
	}

	if claims, ok := token.Claims.(*Claims); ok && token.Valid {
		return claims, nil
	}

	return nil, errors.New("invalid token")
}

func (s *AuthService) GetUserByID(id uint) (*models.User, error) {
	var user models.User
	if err := s.db.First(&user, id).Error; err != nil {
		return nil, err
	}
	return &user, nil
}

func (s *AuthService) CreateAdmin(username, email, password string) error {
	var count int64
	s.db.Model(&models.User{}).Where("role = ?", models.UserRoleAdmin).Count(&count)
	if count > 0 {
		return errors.New("管理员账户已存在")
	}

	hashedPassword, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return err
	}

	user := models.User{
		Username:     username,
		Email:        email,
		PasswordHash: string(hashedPassword),
		Nickname:     username,
		Role:         models.UserRoleAdmin,
		IsActive:     true,
	}

	return s.db.Create(&user).Error
}

func (s *AuthService) ChangePassword(userID uint, oldPassword, newPassword string) error {
	var user models.User
	if err := s.db.First(&user, userID).Error; err != nil {
		return err
	}

	if err := bcrypt.CompareHashAndPassword([]byte(user.PasswordHash), []byte(oldPassword)); err != nil {
		return errors.New("原密码错误")
	}

	hashedPassword, err := bcrypt.GenerateFromPassword([]byte(newPassword), bcrypt.DefaultCost)
	if err != nil {
		return err
	}

	return s.db.Model(&user).Update("password_hash", string(hashedPassword)).Error
}
