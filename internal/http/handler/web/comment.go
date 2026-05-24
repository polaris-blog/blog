package web

import (
	"encoding/json"
	"net/http"
	"strings"

	"github.com/polaris-blog/blog/internal/service"
)

type CommentHandler struct {
	commentService *service.CommentService
}

func NewCommentHandler(commentService *service.CommentService) *CommentHandler {
	return &CommentHandler{commentService: commentService}
}

func (h *CommentHandler) Create(w http.ResponseWriter, r *http.Request) {
	var input service.CreateCommentInput
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}

	input.Content = strings.TrimSpace(input.Content)
	input.AuthorName = strings.TrimSpace(input.AuthorName)
	input.AuthorEmail = strings.TrimSpace(input.AuthorEmail)

	if input.Content == "" {
		http.Error(w, "content is required", http.StatusBadRequest)
		return
	}
	if len(input.Content) > 10000 {
		http.Error(w, "content too long (max 10000 characters)", http.StatusBadRequest)
		return
	}
	if input.AuthorName == "" {
		http.Error(w, "author_name is required", http.StatusBadRequest)
		return
	}
	if len(input.AuthorName) > 100 {
		http.Error(w, "author_name too long", http.StatusBadRequest)
		return
	}
	if input.AuthorEmail != "" && len(input.AuthorEmail) > 254 {
		http.Error(w, "author_email too long", http.StatusBadRequest)
		return
	}

	input.IP = extractIP(r)
	input.UserAgent = r.UserAgent()

	comment, err := h.commentService.Create(r.Context(), input)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusCreated)
	json.NewEncoder(w).Encode(comment)
}

func extractIP(r *http.Request) string {
	ip := r.RemoteAddr
	if idx := strings.LastIndex(ip, ":"); idx != -1 {
		ip = ip[:idx]
	}
	return ip
}
