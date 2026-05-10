package models

import (
	"time"

	"gorm.io/gorm"
)

type BaseModel struct {
	ID        uint           `gorm:"primarykey" json:"id"`
	CreatedAt time.Time      `json:"created_at"`
	UpdatedAt time.Time      `json:"updated_at"`
	DeletedAt gorm.DeletedAt `gorm:"index" json:"-"`
}

type User struct {
	BaseModel
	Username     string `gorm:"uniqueIndex;size:50;not null" json:"username"`
	Email        string `gorm:"uniqueIndex;size:100;not null" json:"email"`
	PasswordHash string `gorm:"size:255;not null" json:"-"`
	Nickname     string `gorm:"size:50" json:"nickname"`
	Avatar       string `gorm:"size:255" json:"avatar"`
	Bio          string `gorm:"size:500" json:"bio"`
	Role         string `gorm:"size:20;default:admin" json:"role"`
	IsActive     bool   `gorm:"default:true" json:"is_active"`
}

type Post struct {
	BaseModel
	Title       string `gorm:"size:200;not null" json:"title"`
	Slug        string `gorm:"uniqueIndex;size:200" json:"slug"`
	Content     string `gorm:"type:text" json:"content"`
	Excerpt     string `gorm:"size:500" json:"excerpt"`
	CoverImage  string `gorm:"size:255" json:"cover_image"`
	Status      string `gorm:"size:20;default:draft" json:"status"`
	IsTop       bool   `gorm:"default:false" json:"is_top"`
	Password    string `gorm:"size:100" json:"-"`
	ViewCount   int    `gorm:"default:0" json:"view_count"`
	PublishedAt *time.Time `json:"published_at"`
	AuthorID    uint  `json:"author_id"`
	Author      User   `json:"author"`
	CategoryID  *uint  `json:"category_id"`
	Category    *Category `json:"category,omitempty"`
	Tags        []Tag  `gorm:"many2many:post_tags;" json:"tags"`
}

type PostVersion struct {
	BaseModel
	PostID    uint      `gorm:"not null;index" json:"post_id"`
	Title     string    `gorm:"size:200;not null" json:"title"`
	Content   string    `gorm:"type:text" json:"content"`
	Version   int       `gorm:"not null" json:"version"`
	CreatedAt time.Time `json:"created_at"`
}

type Category struct {
	BaseModel
	Name        string `gorm:"size:50;not null" json:"name"`
	Slug        string `gorm:"uniqueIndex;size:50" json:"slug"`
	Description string `gorm:"size:200" json:"description"`
	SortOrder   int    `gorm:"default:0" json:"sort_order"`
	PostCount   int    `gorm:"default:0" json:"post_count"`
}

type Tag struct {
	BaseModel
	Name      string `gorm:"size:50;not null" json:"name"`
	Slug      string `gorm:"uniqueIndex;size:50" json:"slug"`
	PostCount int    `gorm:"default:0" json:"post_count"`
}

type Comment struct {
	BaseModel
	PostID      uint       `gorm:"not null;index" json:"post_id"`
	Post        Post       `gorm:"foreignKey:PostID" json:"post"`
	ParentID    *uint      `json:"parent_id"`
	Parent      *Comment   `gorm:"foreignKey:ParentID" json:"parent,omitempty"`
	Replies     []Comment  `gorm:"foreignKey:ParentID" json:"replies,omitempty"`
	AuthorName  string     `gorm:"size:50;not null" json:"author_name"`
	AuthorEmail string     `gorm:"size:100;not null" json:"author_email"`
	AuthorURL   string     `gorm:"size:255" json:"author_url"`
	Content     string     `gorm:"type:text;not null" json:"content"`
	Status      string     `gorm:"size:20;default:pending" json:"status"`
	LikeCount   int        `gorm:"default:0" json:"like_count"`
	IP          string     `gorm:"size:45" json:"-"`
	UserAgent   string     `gorm:"size:500" json:"-"`
}

type CommentLike struct {
	ID         uint      `gorm:"primarykey" json:"id"`
	CommentID  uint      `gorm:"not null;uniqueIndex:idx_comment_ip" json:"comment_id"`
	IP         string    `gorm:"size:45;uniqueIndex:idx_comment_ip" json:"-"`
	CreatedAt  time.Time `json:"created_at"`
}

type Media struct {
	BaseModel
	Name       string `gorm:"size:255;not null" json:"name"`
	Path       string `gorm:"size:500;not null" json:"path"`
	URL        string `gorm:"size:500;not null" json:"url"`
	Type       string `gorm:"size:50" json:"type"`
	Size       int64  `json:"size"`
	MimeType   string `gorm:"size:100" json:"mime_type"`
	Width      int    `json:"width"`
	Height     int    `json:"height"`
	Thumbnail  string `gorm:"size:500" json:"thumbnail"`
}

type FriendLink struct {
	BaseModel
	Name        string `gorm:"size:50;not null" json:"name"`
	URL         string `gorm:"size:255;not null" json:"url"`
	Logo        string `gorm:"size:255" json:"logo"`
	Description string `gorm:"size:200" json:"description"`
	SortOrder   int    `gorm:"default:0" json:"sort_order"`
	IsActive    bool   `gorm:"default:true" json:"is_active"`
}

type Setting struct {
	ID        uint      `gorm:"primarykey" json:"id"`
	Key       string    `gorm:"uniqueIndex;size:50;not null" json:"key"`
	Value     string    `gorm:"type:text" json:"value"`
	UpdatedAt time.Time `json:"updated_at"`
}

type Navigation struct {
	BaseModel
	Name      string `gorm:"size:50;not null" json:"name"`
	URL       string `gorm:"size:255;not null" json:"url"`
	Icon      string `gorm:"size:100" json:"icon"`
	Target    string `gorm:"size:20;default:_self" json:"target"`
	SortOrder int    `gorm:"default:0" json:"sort_order"`
	IsActive  bool   `gorm:"default:true" json:"is_active"`
	IsBuiltin bool   `gorm:"default:false" json:"is_builtin"`
	ParentID  *uint  `json:"parent_id"`
	Children  []Navigation `gorm:"foreignKey:ParentID" json:"children,omitempty"`
}

type Page struct {
	BaseModel
	Title       string     `gorm:"size:200;not null" json:"title"`
	Slug        string     `gorm:"uniqueIndex;size:200" json:"slug"`
	Content     string     `gorm:"type:text" json:"content"`
	Excerpt     string     `gorm:"size:500" json:"excerpt"`
	CoverImage  string     `gorm:"size:255" json:"cover_image"`
	Status      string     `gorm:"size:20;default:draft" json:"status"`
	IsInNav     bool       `gorm:"default:false" json:"is_in_nav"`
	NavSort     int        `gorm:"default:0" json:"nav_sort"`
	ViewCount   int        `gorm:"default:0" json:"view_count"`
	PublishedAt *time.Time `json:"published_at"`
	AuthorID    uint       `json:"author_id"`
	Author      User       `json:"author"`
}

const (
	PageStatusDraft     = "draft"
	PageStatusPublished = "published"
)

type Theme struct {
	BaseModel
	Name        string `gorm:"size:50;not null" json:"name"`
	Slug        string `gorm:"uniqueIndex;size:50" json:"slug"`
	Version     string `gorm:"size:20" json:"version"`
	Author      string `gorm:"size:50" json:"author"`
	Description string `gorm:"size:500" json:"description"`
	Path        string `gorm:"size:255;not null" json:"path"`
	IsActive    bool   `gorm:"default:false" json:"is_active"`
	Config      string `gorm:"type:text" json:"config"`
}

type Plugin struct {
	BaseModel
	Name        string `gorm:"size:50;not null" json:"name"`
	Slug        string `gorm:"uniqueIndex;size:50" json:"slug"`
	Version     string `gorm:"size:20" json:"version"`
	Author      string `gorm:"size:50" json:"author"`
	Description string `gorm:"size:500" json:"description"`
	Path        string `gorm:"size:255;not null" json:"path"`
	IsActive    bool   `gorm:"default:false" json:"is_active"`
	Permissions string `gorm:"type:text" json:"permissions"`
	Config      string `gorm:"type:text" json:"config"`
}

type LoginAttempt struct {
	ID        uint      `gorm:"primarykey" json:"id"`
	IP        string    `gorm:"size:45;not null;index" json:"ip"`
	Username  string    `gorm:"size:50;not null" json:"username"`
	Success   bool      `gorm:"default:false" json:"success"`
	CreatedAt time.Time `gorm:"index" json:"created_at"`
}

const (
	PostStatusDraft     = "draft"
	PostStatusPublished = "published"
	PostStatusScheduled = "scheduled"

	CommentStatusPending  = "pending"
	CommentStatusApproved = "approved"
	CommentStatusSpam     = "spam"

	UserRoleAdmin = "admin"
)
