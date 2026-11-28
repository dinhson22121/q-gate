package main

import (
	"encoding/json"
	"log"
	"net/http"
)

type Order struct {
	ID     string `json:"id"`
	UserID string `json:"user_id"`
	Total  int    `json:"total"`
}

var orders = map[string]Order{}

func userID(r *http.Request) string {
	return r.Header.Get("X-Authenticated-User")
}

func listOrders(w http.ResponseWriter, r *http.Request) {
	mine := []Order{}
	for _, order := range orders {
		if order.UserID == userID(r) {
			mine = append(mine, order)
		}
	}
	json.NewEncoder(w).Encode(mine)
}

func main() {
	http.HandleFunc("/orders", listOrders)
	log.Fatal(http.ListenAndServe(":8080", nil))
}
