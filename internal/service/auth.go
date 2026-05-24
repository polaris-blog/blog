package service

import (
	"context"
	"fmt"
	"time"

	"github.com/golang-jwt/jwt/v5"
	"github.com/google/uuid"
	"github.com/polaris-blog/blog/internal/config"
	"github.com/polaris-blog/blog/internal/http/middleware"
	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/plugin"
	"github.com/polaris-blog/blog/internal/repository"
	"golang.org/x/crypto/bcrypt"
)

type AuthService struct {
	users    repository.UserRepository
	config   config.SecurityConfig
	eventBus *plugin.EventBus
}

func NewAuthService(users repository.UserRepository, config config.SecurityConfig, eventBus *plugin.EventBus) *AuthService {
	return &AuthService{users: users, config: config, eventBus: eventBus}
}

type TokenPair struct {
	AccessToken  string `json:"access_token"`
	RefreshToken string `json:"refresh_token"`
	ExpiresAt    int64  `json:"expires_at"`
}

func (s *AuthService) Login(ctx context.Context, username, password string) (*model.User, *TokenPair, error) {
	user, err := s.users.FindByUsername(ctx, username)
	if err != nil {
		return nil, nil, fmt.Errorf("invalid credentials")
	}

	if user.Status != model.StatusActive {
		return nil, nil, fmt.Errorf("user account is inactive")
	}

	if err := bcrypt.CompareHashAndPassword([]byte(user.PasswordHash), []byte(password)); err != nil {
		return nil, nil, fmt.Errorf("invalid credentials")
	}

	tokens, err := s.generateTokenPair(user)
	if err != nil {
		return nil, nil, fmt.Errorf("generate token: %w", err)
	}

	_ = s.users.UpdateLastLogin(ctx, user.ID)
	_ = s.eventBus.EmitHook(ctx, plugin.HookUserAfterLogin, user)

	return user, tokens, nil
}

func (s *AuthService) ValidateToken(tokenString string) (*middleware.Claims, error) {
	token, err := jwt.ParseWithClaims(tokenString, &middleware.Claims{}, func(token *jwt.Token) (interface{}, error) {
		if _, ok := token.Method.(*jwt.SigningMethodHMAC); !ok {
			return nil, fmt.Errorf("unexpected signing method: %v", token.Header["alg"])
		}
		return []byte(s.config.SecretKey), nil
	})
	if err != nil {
		return nil, err
	}

	claims, ok := token.Claims.(*middleware.Claims)
	if !ok || !token.Valid {
		return nil, fmt.Errorf("invalid token")
	}

	return claims, nil
}

func (s *AuthService) RefreshToken(ctx context.Context, refreshToken string) (*TokenPair, error) {
	claims, err := s.ValidateToken(refreshToken)
	if err != nil {
		return nil, fmt.Errorf("invalid refresh token")
	}

	user, err := s.users.FindByID(ctx, claims.UserID)
	if err != nil {
		return nil, fmt.Errorf("user not found")
	}

	return s.generateTokenPair(user)
}

func (s *AuthService) GetUserByID(ctx context.Context, id string) (*model.User, error) {
	return s.users.FindByID(ctx, id)
}

func (s *AuthService) HasAdmin(ctx context.Context) bool {
	users, err := s.users.List(ctx, repository.ListOptions{Page: 1, PageSize: 1, Filters: map[string]interface{}{"role": "admin"}})
	if err != nil {
		return false
	}
	return len(users.Items) > 0
}

func (s *AuthService) InitAdmin(ctx context.Context, username, email, password string) (*model.User, error) {
	if _, err := s.users.FindByUsername(ctx, username); err == nil {
		return nil, fmt.Errorf("admin already exists")
	}

	hash, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return nil, fmt.Errorf("hash password: %w", err)
	}

	user := &model.User{
		ID:           uuid.New().String(),
		Username:     username,
		Email:        email,
		PasswordHash: string(hash),
		DisplayName:  "Admin",
		Role:         model.RoleAdmin,
		Status:       model.StatusActive,
		CreatedAt:    time.Now(),
		UpdatedAt:    time.Now(),
	}

	if err := s.users.Create(ctx, user); err != nil {
		return nil, fmt.Errorf("create admin: %w", err)
	}

	return user, nil
}

func (s *AuthService) generateTokenPair(user *model.User) (*TokenPair, error) {
	now := time.Now()
	accessExp := now.Add(2 * time.Hour)
	refreshExp := now.Add(7 * 24 * time.Hour)

	accessClaims := &middleware.Claims{
		UserID:   user.ID,
		Username: user.Username,
		Role:     user.Role,
		RegisteredClaims: jwt.RegisteredClaims{
			ExpiresAt: jwt.NewNumericDate(accessExp),
			IssuedAt:  jwt.NewNumericDate(now),
			Issuer:    "polaris",
		},
	}

	accessToken, err := jwt.NewWithClaims(jwt.SigningMethodHS256, accessClaims).SignedString([]byte(s.config.SecretKey))
	if err != nil {
		return nil, err
	}

	refreshClaims := &middleware.Claims{
		UserID:   user.ID,
		Username: user.Username,
		Role:     user.Role,
		RegisteredClaims: jwt.RegisteredClaims{
			ExpiresAt: jwt.NewNumericDate(refreshExp),
			IssuedAt:  jwt.NewNumericDate(now),
			Issuer:    "polaris",
		},
	}

	refreshToken, err := jwt.NewWithClaims(jwt.SigningMethodHS256, refreshClaims).SignedString([]byte(s.config.SecretKey))
	if err != nil {
		return nil, err
	}

	return &TokenPair{
		AccessToken:  accessToken,
		RefreshToken: refreshToken,
		ExpiresAt:    accessExp.Unix(),
	}, nil
}
