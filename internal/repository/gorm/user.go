package gorm

import (
	"context"

	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/repository"
	"gorm.io/gorm"
)

type UserRepo struct {
	db *gorm.DB
}

func NewUserRepo(db *gorm.DB) *UserRepo {
	return &UserRepo{db: db}
}

func (r *UserRepo) FindByID(ctx context.Context, id string) (*model.User, error) {
	var user model.User
	err := r.db.WithContext(ctx).First(&user, "id = ?", id).Error
	if err != nil {
		return nil, err
	}
	return &user, nil
}

func (r *UserRepo) FindByUsername(ctx context.Context, username string) (*model.User, error) {
	var user model.User
	err := r.db.WithContext(ctx).First(&user, "username = ?", username).Error
	if err != nil {
		return nil, err
	}
	return &user, nil
}

func (r *UserRepo) FindByEmail(ctx context.Context, email string) (*model.User, error) {
	var user model.User
	err := r.db.WithContext(ctx).First(&user, "email = ?", email).Error
	if err != nil {
		return nil, err
	}
	return &user, nil
}

func (r *UserRepo) List(ctx context.Context, opts repository.ListOptions) (*repository.ListResult[model.User], error) {
	var users []*model.User
	var total int64

	query := r.db.WithContext(ctx).Model(&model.User{})
	if err := query.Count(&total).Error; err != nil {
		return nil, err
	}

	query = applyPagination(query, opts)
	query = applyOrder(query, opts)

	err := query.Find(&users).Error
	if err != nil {
		return nil, err
	}

	return &repository.ListResult[model.User]{Items: users, Total: total}, nil
}

func (r *UserRepo) Create(ctx context.Context, user *model.User) error {
	return r.db.WithContext(ctx).Create(user).Error
}

func (r *UserRepo) Update(ctx context.Context, user *model.User) error {
	return r.db.WithContext(ctx).Save(user).Error
}

func (r *UserRepo) Delete(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Delete(&model.User{}, "id = ?", id).Error
}

func (r *UserRepo) UpdateLastLogin(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Model(&model.User{}).Where("id = ?", id).
		Update("last_login_at", gorm.Expr("NOW()")).Error
}
