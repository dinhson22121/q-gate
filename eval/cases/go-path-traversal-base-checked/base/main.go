package main

import (
	"log"
	"net/http"
)

func health(w http.ResponseWriter, r *http.Request) {
	w.Write([]byte("ok"))
}

func main() {
	http.HandleFunc("/health", health)
	log.Fatal(http.ListenAndServe(":8080", nil))
}
