package model

import (
	"time"
)

type User struct {
	ID           string    `gorm:"primaryKey;size:36" json:"id"`
	Username     string    `gorm:"size:50;uniqueIndex;not null" json:"username"`
	Email        string    `gorm:"size:100;uniqueIndex;not null" json:"email"`
	PasswordHash string    `gorm:"size:255;not null" json:"-"`
	DisplayName  string    `gorm:"size:100" json:"display_name"`
	Bio          string    `gorm:"size:500" json:"bio"`
	Avatar       string    `gorm:"size:500" json:"avatar"`
	Role         string    `gorm:"size:20;default:author;not null" json:"role"`
	Status       string    `gorm:"size:20;default:active;not null" json:"status"`
	LastLoginAt  *time.Time `json:"last_login_at"`
	CreatedAt    time.Time `json:"created_at"`
	UpdatedAt    time.Time `json:"updated_at"`
}

func (User) TableName() string { return "users" }

const (
	RoleAdmin  = "admin"
	RoleEditor = "editor"
	RoleAuthor = "author"

	StatusActive   = "active"
	StatusInactive = "inactive"
)
