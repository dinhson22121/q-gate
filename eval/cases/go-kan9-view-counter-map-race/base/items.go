package main

import (
	"encoding/json"
	"errors"
	"net/http"
	"strings"
	"sync"
)

const maxItemBodyBytes = 1 << 20

type Item struct {
	ID    int64   `json:"id"`
	Name  string  `json:"name"`
	Price float64 `json:"price"`
}

type ItemStore struct {
	mu     sync.Mutex
	nextID int64
	items  map[int64]Item
}

func NewItemStore() *ItemStore {
	return &ItemStore{items: map[int64]Item{}}
}

func (s *ItemStore) Add(name string, price float64) Item {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.nextID++
	item := Item{ID: s.nextID, Name: name, Price: price}
	s.items[item.ID] = item
	return item
}

type newItemRequest struct {
	Name  string  `json:"name"`
	Price float64 `json:"price"`
}

func (r newItemRequest) validate() error {
	if strings.TrimSpace(r.Name) == "" {
		return errors.New("name is required")
	}
	if r.Price <= 0 {
		return errors.New("price must be greater than zero")
	}
	return nil
}

func writeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(body)
}

func writeError(w http.ResponseWriter, status int, message string) {
	writeJSON(w, status, map[string]string{"error": message})
}

func addItemHandler(store *ItemStore) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			w.Header().Set("Allow", http.MethodPost)
			writeError(w, http.StatusMethodNotAllowed, "method not allowed")
			return
		}
		var req newItemRequest
		decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxItemBodyBytes))
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, "invalid JSON body")
			return
		}
		if err := req.validate(); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, store.Add(strings.TrimSpace(req.Name), req.Price))
	}
}
