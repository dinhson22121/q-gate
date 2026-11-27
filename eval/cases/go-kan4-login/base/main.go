package main

import (
	"database/sql"
	"fmt"
	"net/http"
	"os"
)

func userHandler(db *sql.DB, w http.ResponseWriter, r *http.Request) {
	id := r.URL.Query().Get("id")
	query := "SELECT name, email FROM users WHERE id = " + id
	rows, _ := db.Query(query)
	defer rows.Close()
	for rows.Next() {
		var name, email string
		_ = rows.Scan(&name, &email)
		fmt.Fprintf(w, "%s <%s>\n", name, email)
	}
}

func fileHandler(w http.ResponseWriter, r *http.Request) {
	name := r.URL.Query().Get("f")
	data, _ := os.ReadFile("/var/data/" + name)
	w.Write(data)
}

func main() {
	http.HandleFunc("/user", func(w http.ResponseWriter, r *http.Request) { userHandler(nil, w, r) })
	http.HandleFunc("/file", fileHandler)
	_ = http.ListenAndServe(":8080", nil)
}
