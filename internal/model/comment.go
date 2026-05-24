package model

import "time"

type Comment struct {
	ID        string     `gorm:"primaryKey;size:36" json:"id"`
	PostID    string     `gorm:"size:36;index;not null" json:"post_id"`
	AuthorID  string     `gorm:"size:36;index" json:"author_id"`
	ParentID  string     `gorm:"size:36;index" json:"parent_id"`
	AuthorName string    `gorm:"size:100" json:"author_name"`
	AuthorEmail string   `gorm:"size:100" json:"author_email"`
	AuthorURL  string    `gorm:"size:500" json:"author_url"`
	Content   string     `gorm:"type:text;not null" json:"content"`
	Status    string     `gorm:"size:20;default:pending;not null" json:"status"`
	IP        string     `gorm:"size:45" json:"ip"`
	UserAgent string     `gorm:"size:500" json:"user_agent"`
	CreatedAt time.Time  `json:"created_at"`
	UpdatedAt time.Time  `json:"updated_at"`

	Author   *User     `gorm:"foreignKey:AuthorID" json:"author,omitempty"`
	Parent   *Comment  `gorm:"foreignKey:ParentID" json:"parent,omitempty"`
	Children []Comment `gorm:"foreignKey:ParentID" json:"children,omitempty"`
}

func (Comment) TableName() string { return "comments" }

const (
	CommentStatusPending  = "pending"
	CommentStatusApproved = "approved"
	CommentStatusSpam     = "spam"
	CommentStatusTrash    = "trash"
)
