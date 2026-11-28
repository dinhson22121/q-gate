package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func postItem(t *testing.T, store *ItemStore, method, body string) *httptest.ResponseRecorder {
	t.Helper()
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(method, "/items", strings.NewReader(body))
	addItemHandler(store)(rec, req)
	return rec
}

func TestAddItemCreatesItemWithGeneratedID(t *testing.T) {
	store := NewItemStore()
	rec := postItem(t, store, http.MethodPost, `{"name":"  Mug ","price":9.5}`)
	if rec.Code != http.StatusCreated {
		t.Fatalf("got %d %s", rec.Code, rec.Body.String())
	}
	var got Item
	if err := json.Unmarshal(rec.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got != (Item{ID: 1, Name: "Mug", Price: 9.5}) {
		t.Fatalf("got %+v", got)
	}
	if stored := store.items[1]; stored != got {
		t.Fatalf("stored %+v", stored)
	}
	second := postItem(t, store, http.MethodPost, `{"name":"Cup","price":1}`)
	if !strings.Contains(second.Body.String(), `"id":2`) {
		t.Fatalf("second item: %s", second.Body.String())
	}
}

func TestAddItemRejectsInvalidInput(t *testing.T) {
	cases := map[string]string{
		"empty name":     `{"name":"  ","price":3}`,
		"zero price":     `{"name":"Mug","price":0}`,
		"negative price": `{"name":"Mug","price":-1}`,
		"malformed":      `{"name":`,
		"unknown field":  `{"name":"Mug","price":3,"id":7}`,
	}
	for name, body := range cases {
		store := NewItemStore()
		rec := postItem(t, store, http.MethodPost, body)
		if rec.Code != http.StatusBadRequest || !strings.Contains(rec.Body.String(), `"error"`) {
			t.Errorf("%s: got %d %s", name, rec.Code, rec.Body.String())
		}
		if len(store.items) != 0 {
			t.Errorf("%s: item was saved", name)
		}
	}
}

func TestAddItemOnlyAllowsPost(t *testing.T) {
	for _, method := range []string{http.MethodGet, http.MethodPut, http.MethodDelete} {
		rec := postItem(t, NewItemStore(), method, `{"name":"Mug","price":3}`)
		if rec.Code != http.StatusMethodNotAllowed || rec.Header().Get("Allow") != http.MethodPost {
			t.Errorf("%s: got %d", method, rec.Code)
		}
	}
}
