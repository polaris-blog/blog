package model

import "time"

type Media struct {
	ID        string    `gorm:"primaryKey;size:36" json:"id"`
	Name      string    `gorm:"size:255;not null" json:"name"`
	Path      string    `gorm:"size:500;not null" json:"path"`
	URL       string    `gorm:"size:500;not null" json:"url"`
	MimeType  string    `gorm:"size:100;not null" json:"mime_type"`
	Size      int64     `json:"size"`
	Width     int       `json:"width"`
	Height    int       `json:"height"`
	UploaderID string   `gorm:"size:36;index;not null" json:"uploader_id"`
	CreatedAt time.Time `json:"created_at"`

	Uploader User `gorm:"foreignKey:UploaderID" json:"uploader,omitempty"`
}

func (Media) TableName() string { return "media" }
