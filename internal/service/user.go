package service

import (
	"context"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/plugin"
	"github.com/polaris-blog/blog/internal/repository"
	"golang.org/x/crypto/bcrypt"
)

type UserService struct {
	users    repository.UserRepository
	eventBus *plugin.EventBus
}

func NewUserService(users repository.UserRepository, eventBus *plugin.EventBus) *UserService {
	return &UserService{users: users, eventBus: eventBus}
}

func (s *UserService) GetByID(ctx context.Context, id string) (*model.User, error) {
	return s.users.FindByID(ctx, id)
}

func (s *UserService) GetByUsername(ctx context.Context, username string) (*model.User, error) {
	return s.users.FindByUsername(ctx, username)
}

func (s *UserService) List(ctx context.Context, page, pageSize int) ([]*model.User, int64, error) {
	opts := repository.ListOptions{
		Page:     page,
		PageSize: pageSize,
		OrderBy:  "created_at",
		Order:    "DESC",
	}
	result, err := s.users.List(ctx, opts)
	if err != nil {
		return nil, 0, err
	}
	return result.Items, result.Total, nil
}

type CreateUserInput struct {
	Username    string `json:"username"`
	Email       string `json:"email"`
	Password    string `json:"password"`
	DisplayName string `json:"display_name"`
	Bio         string `json:"bio"`
	Role        string `json:"role"`
}

func (s *UserService) Create(ctx context.Context, input CreateUserInput) (*model.User, error) {
	if _, err := s.users.FindByUsername(ctx, input.Username); err == nil {
		return nil, fmt.Errorf("username already exists")
	}
	if _, err := s.users.FindByEmail(ctx, input.Email); err == nil {
		return nil, fmt.Errorf("email already exists")
	}

	hash, err := bcrypt.GenerateFromPassword([]byte(input.Password), bcrypt.DefaultCost)
	if err != nil {
		return nil, fmt.Errorf("hash password: %w", err)
	}

	user := &model.User{
		ID:           uuid.New().String(),
		Username:     input.Username,
		Email:        input.Email,
		PasswordHash: string(hash),
		DisplayName:  input.DisplayName,
		Bio:          input.Bio,
		Role:         input.Role,
		Status:       model.StatusActive,
		CreatedAt:    time.Now(),
		UpdatedAt:    time.Now(),
	}

	if user.Role == "" {
		user.Role = model.RoleAuthor
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookUserBeforeCreate, user)

	if err := s.users.Create(ctx, user); err != nil {
		return nil, fmt.Errorf("create user: %w", err)
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookUserAfterCreate, user)

	return user, nil
}

type UpdateUserInput struct {
	DisplayName *string `json:"display_name"`
	Bio         *string `json:"bio"`
	Avatar      *string `json:"avatar"`
	Role        *string `json:"role"`
	Status      *string `json:"status"`
}

func (s *UserService) Update(ctx context.Context, id string, input UpdateUserInput) (*model.User, error) {
	user, err := s.users.FindByID(ctx, id)
	if err != nil {
		return nil, fmt.Errorf("find user: %w", err)
	}

	if input.DisplayName != nil {
		user.DisplayName = *input.DisplayName
	}
	if input.Bio != nil {
		user.Bio = *input.Bio
	}
	if input.Avatar != nil {
		user.Avatar = *input.Avatar
	}
	if input.Role != nil {
		user.Role = *input.Role
	}
	if input.Status != nil {
		user.Status = *input.Status
	}

	user.UpdatedAt = time.Now()

	if err := s.users.Update(ctx, user); err != nil {
		return nil, fmt.Errorf("update user: %w", err)
	}

	return user, nil
}

func (s *UserService) Delete(ctx context.Context, id string) error {
	return s.users.Delete(ctx, id)
}

func (s *UserService) ChangePassword(ctx context.Context, id string, oldPassword, newPassword string) error {
	user, err := s.users.FindByID(ctx, id)
	if err != nil {
		return fmt.Errorf("find user: %w", err)
	}

	if err := bcrypt.CompareHashAndPassword([]byte(user.PasswordHash), []byte(oldPassword)); err != nil {
		return fmt.Errorf("invalid old password")
	}

	hash, err := bcrypt.GenerateFromPassword([]byte(newPassword), bcrypt.DefaultCost)
	if err != nil {
		return fmt.Errorf("hash password: %w", err)
	}

	user.PasswordHash = string(hash)
	user.UpdatedAt = time.Now()

	return s.users.Update(ctx, user)
}

func (s *UserService) UpdateLastLogin(ctx context.Context, id string) error {
	return s.users.UpdateLastLogin(ctx, id)
}
