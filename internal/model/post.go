package model

import (
	"time"
)

type Post struct {
	ID          string    `gorm:"primaryKey;size:36" json:"id"`
	Title       string    `gorm:"size:255;not null" json:"title"`
	Slug        string    `gorm:"size:255;uniqueIndex:idx_slug_type;not null" json:"slug"`
	Content     string    `gorm:"type:text" json:"content"`
	Excerpt     string    `gorm:"size:500" json:"excerpt"`
	Status      string    `gorm:"size:20;default:draft;not null" json:"status"`
	Type        string    `gorm:"size:20;uniqueIndex:idx_slug_type;default:post;not null" json:"type"`
	Format      string    `gorm:"size:20;default:markdown" json:"format"`
	CoverImage  string    `gorm:"size:500" json:"cover_image"`
	AuthorID    string    `gorm:"size:36;index;not null" json:"author_id"`
	CategoryID  string    `gorm:"size:36;index" json:"category_id"`
	IsPinned    bool      `gorm:"default:false" json:"is_pinned"`
	AllowComment bool     `gorm:"default:true" json:"allow_comment"`
	PublishedAt  *time.Time `json:"published_at"`
	CreatedAt   time.Time `json:"created_at"`
	UpdatedAt   time.Time `json:"updated_at"`

	Author     User       `gorm:"foreignKey:AuthorID" json:"author,omitempty"`
	Category   *Category  `gorm:"foreignKey:CategoryID" json:"category,omitempty"`
	Tags       []Tag      `gorm:"many2many:post_tags;" json:"tags,omitempty"`
	Metas      []PostMeta `gorm:"foreignKey:PostID" json:"metas,omitempty"`
}

func (Post) TableName() string { return "posts" }

type PostMeta struct {
	ID        string    `gorm:"primaryKey;size:36" json:"id"`
	PostID    string    `gorm:"size:36;index;not null" json:"post_id"`
	Key       string    `gorm:"size:100;not null" json:"key"`
	Value     string    `gorm:"type:text" json:"value"`
	CreatedAt time.Time `json:"created_at"`
}

func (PostMeta) TableName() string { return "post_metas" }
