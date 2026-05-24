package storage

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/google/uuid"
)

type Storage interface {
	Save(name string, reader io.Reader) (path string, err error)
	Get(path string) (io.ReadCloser, error)
	Delete(path string) error
	URL(path string, baseURL string) string
}

type LocalStorage struct {
	basePath string
}

func NewLocalStorage(basePath string) *LocalStorage {
	return &LocalStorage{basePath: basePath}
}

func (s *LocalStorage) Save(name string, reader io.Reader) (string, error) {
	if err := os.MkdirAll(s.basePath, 0755); err != nil {
		return "", fmt.Errorf("create storage dir: %w", err)
	}

	ext := filepath.Ext(name)
	dateDir := time.Now().Format("2006/01/02")
	dir := filepath.Join(s.basePath, dateDir)

	if err := os.MkdirAll(dir, 0755); err != nil {
		return "", fmt.Errorf("create date dir: %w", err)
	}

	filename := uuid.New().String() + ext
	fullPath := filepath.Join(dir, filename)

	f, err := os.Create(fullPath)
	if err != nil {
		return "", fmt.Errorf("create file: %w", err)
	}
	defer f.Close()

	if _, err := io.Copy(f, reader); err != nil {
		os.Remove(fullPath)
		return "", fmt.Errorf("write file: %w", err)
	}

	relPath := filepath.Join(dateDir, filename)
	return relPath, nil
}

func (s *LocalStorage) safePath(path string) (string, error) {
	if strings.Contains(path, "..") {
		return "", fmt.Errorf("invalid path: path traversal detected")
	}
	fullPath := filepath.Join(s.basePath, path)
	absPath, err := filepath.Abs(fullPath)
	if err != nil {
		return "", fmt.Errorf("resolve path: %w", err)
	}
	absBase, err := filepath.Abs(s.basePath)
	if err != nil {
		return "", fmt.Errorf("resolve base path: %w", err)
	}
	if !strings.HasPrefix(absPath, absBase+string(os.PathSeparator)) && absPath != absBase {
		return "", fmt.Errorf("invalid path: path traversal detected")
	}
	return absPath, nil
}

func (s *LocalStorage) Get(path string) (io.ReadCloser, error) {
	fullPath, err := s.safePath(path)
	if err != nil {
		return nil, err
	}
	f, err := os.Open(fullPath)
	if err != nil {
		return nil, fmt.Errorf("open file: %w", err)
	}
	return f, nil
}

func (s *LocalStorage) Delete(path string) error {
	fullPath, err := s.safePath(path)
	if err != nil {
		return err
	}
	return os.Remove(fullPath)
}

func (s *LocalStorage) URL(path string, baseURL string) string {
	baseURL = strings.TrimRight(baseURL, "/")
	return baseURL + "/uploads/" + strings.ReplaceAll(path, "\\", "/")
}
