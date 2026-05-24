package model

import (
	"time"

	"github.com/google/uuid"
	"gorm.io/gorm"
)

type Option struct {
	ID        string    `gorm:"primaryKey;size:36" json:"id"`
	Key       string    `gorm:"size:100;uniqueIndex;not null" json:"key"`
	Value     string    `gorm:"type:text" json:"value"`
	UpdatedAt time.Time `json:"updated_at"`
}

func (o *Option) BeforeCreate(tx *gorm.DB) error {
	if o.ID == "" {
		o.ID = uuid.New().String()
	}
	return nil
}

func (Option) TableName() string { return "options" }
