package main

import (
	"log"
	"net/http"
)

var sessions = map[string]string{}

func main() {
	log.Fatal(http.ListenAndServe(":8080", nil))
}
