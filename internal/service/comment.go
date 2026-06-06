package service

import (
	"context"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/polaris-blog/blog/internal/model"
	"github.com/polaris-blog/blog/internal/plugin"
	"github.com/polaris-blog/blog/internal/repository"
)

type CommentService struct {
	comments repository.CommentRepository
	posts    repository.PostRepository
	eventBus *plugin.EventBus
}

func NewCommentService(comments repository.CommentRepository, posts repository.PostRepository, eventBus *plugin.EventBus) *CommentService {
	return &CommentService{comments: comments, posts: posts, eventBus: eventBus}
}

type CreateCommentInput struct {
	PostID      string `json:"post_id"`
	ParentID    string `json:"parent_id"`
	AuthorName  string `json:"author_name"`
	AuthorEmail string `json:"author_email"`
	AuthorURL   string `json:"author_url"`
	Content     string `json:"content"`
	IP          string `json:"-"`
	UserAgent   string `json:"-"`
}

func (s *CommentService) Create(ctx context.Context, input CreateCommentInput) (*model.Comment, error) {
	post, err := s.posts.FindByID(ctx, input.PostID)
	if err != nil {
		return nil, fmt.Errorf("post not found")
	}
	if !post.AllowComment {
		return nil, fmt.Errorf("comments are disabled for this post")
	}

	if input.ParentID != "" {
		parent, err := s.comments.FindByID(ctx, input.ParentID)
		if err != nil {
			return nil, fmt.Errorf("parent comment not found")
		}
		if parent.PostID != input.PostID {
			return nil, fmt.Errorf("parent comment does not belong to this post")
		}
	}

	comment := &model.Comment{
		ID:          uuid.New().String(),
		PostID:      input.PostID,
		ParentID:    input.ParentID,
		AuthorName:  input.AuthorName,
		AuthorEmail: input.AuthorEmail,
		AuthorURL:   input.AuthorURL,
		Content:     input.Content,
		Status:      model.CommentStatusPending,
		IP:          input.IP,
		UserAgent:   input.UserAgent,
		CreatedAt:   time.Now(),
		UpdatedAt:   time.Now(),
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookCommentBeforeCreate, comment)

	if err := s.comments.Create(ctx, comment); err != nil {
		return nil, fmt.Errorf("create comment: %w", err)
	}

	_ = s.eventBus.EmitHook(ctx, plugin.HookCommentAfterCreate, comment)

	return comment, nil
}

func (s *CommentService) GetByPost(ctx context.Context, postID string) ([]*model.Comment, error) {
	opts := repository.ListOptions{
		Page:     1,
		PageSize: 1000,
		OrderBy:  "created_at",
		Order:    "ASC",
		Status:   model.CommentStatusApproved,
	}
	result, err := s.comments.FindByPost(ctx, postID, opts)
	if err != nil {
		return nil, err
	}
	return result.Items, nil
}

func (s *CommentService) List(ctx context.Context, status string, page, pageSize int) ([]*model.Comment, int64, error) {
	opts := repository.ListOptions{
		Page:     page,
		PageSize: pageSize,
		OrderBy:  "created_at",
		Order:    "DESC",
		Status:   status,
	}
	result, err := s.comments.List(ctx, opts)
	if err != nil {
		return nil, 0, err
	}
	return result.Items, result.Total, nil
}

func (s *CommentService) Approve(ctx context.Context, id string) error {
	return s.comments.UpdateStatus(ctx, id, model.CommentStatusApproved)
}

func (s *CommentService) MarkSpam(ctx context.Context, id string) error {
	return s.comments.UpdateStatus(ctx, id, model.CommentStatusSpam)
}

func (s *CommentService) Delete(ctx context.Context, id string) error {
	return s.comments.Delete(ctx, id)
}

func (s *CommentService) CountByStatus(ctx context.Context, status string) (int64, error) {
	return s.comments.CountByStatus(ctx, status)
}

func (s *CommentService) Count(ctx context.Context, status string) (int64, error) {
	return s.CountByStatus(ctx, status)
}

func (s *CommentService) BuildTree(comments []*model.Comment) []*CommentNode {
	nodeMap := make(map[string]*CommentNode)
	var roots []*CommentNode

	for _, c := range comments {
		nodeMap[c.ID] = &CommentNode{Comment: c}
	}

	for _, c := range comments {
		node := nodeMap[c.ID]
		if c.ParentID == "" {
			roots = append(roots, node)
		} else {
			if parent, ok := nodeMap[c.ParentID]; ok {
				if hasCycle(nodeMap, c.ID, c.ParentID) {
					roots = append(roots, node)
				} else {
					parent.Children = append(parent.Children, node)
				}
			} else {
				roots = append(roots, node)
			}
		}
	}

	return roots
}

func hasCycle(nodeMap map[string]*CommentNode, startID, targetParentID string) bool {
	visited := make(map[string]bool)
	current := targetParentID
	for current != "" {
		if current == startID {
			return true
		}
		if visited[current] {
			return true
		}
		visited[current] = true
		if node, ok := nodeMap[current]; ok && node.Comment.ParentID != "" {
			current = node.Comment.ParentID
		} else {
			break
		}
	}
	return false
}

type CommentNode struct {
	Comment  *model.Comment
	Children []*CommentNode
}

type FlatComment struct {
	Comment     *model.Comment
	Depth       int
	IndentPx    int
	ChildCount  int
	HasChildren bool
}

func (s *CommentService) FlattenTree(nodes []*CommentNode) []FlatComment {
	var result []FlatComment
	const maxDepth = 20
	var walk func([]*CommentNode, int)
	walk = func(nodes []*CommentNode, depth int) {
		if depth > maxDepth {
			return
		}
		for _, n := range nodes {
			cc := len(n.Children)
			result = append(result, FlatComment{
				Comment:     n.Comment,
				Depth:       depth,
				IndentPx:    depth * 40,
				ChildCount:  cc,
				HasChildren: cc > 0,
			})
			if cc > 0 {
				walk(n.Children, depth+1)
			}
		}
	}
	walk(nodes, 0)
	return result
}
