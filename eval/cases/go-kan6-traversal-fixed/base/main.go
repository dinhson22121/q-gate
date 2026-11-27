package main

import (
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"database/sql"
	"encoding/hex"
	"fmt"
	"net/http"
	"os"
	"strconv"
	"sync"
)

type credentials struct {
	salt []byte
	hash []byte
}

var users = map[string]credentials{}

var sessions = struct {
	sync.Mutex
	byID map[string]string
}{byID: map[string]string{}}

func passwordHash(salt []byte, password string) []byte {
	h := sha256.New()
	h.Write(salt)
	h.Write([]byte(password))
	return h.Sum(nil)
}

func registerUser(email, password string) {
	salt := make([]byte, 16)
	_, _ = rand.Read(salt)
	users[email] = credentials{salt: salt, hash: passwordHash(salt, password)}
}

func newSessionID() (string, error) {
	raw := make([]byte, 16)
	if _, err := rand.Read(raw); err != nil {
		return "", err
	}
	return hex.EncodeToString(raw), nil
}

func sessionCookie(value string, maxAge int) *http.Cookie {
	return &http.Cookie{
		Name:     "session",
		Value:    value,
		Path:     "/",
		MaxAge:   maxAge,
		HttpOnly: true,
		Secure:   true,
		SameSite: http.SameSiteStrictMode,
	}
}

func loginHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return
	}
	email := r.FormValue("email")
	password := r.FormValue("password")

	cred, known := users[email]
	if !known {
		cred = credentials{salt: make([]byte, 16), hash: make([]byte, sha256.Size)}
	}
	match := subtle.ConstantTimeCompare(cred.hash, passwordHash(cred.salt, password)) == 1
	if !known || !match {
		http.Error(w, "invalid email or password", http.StatusUnauthorized)
		return
	}

	id, err := newSessionID()
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	sessions.Lock()
	sessions.byID[id] = email
	sessions.Unlock()

	http.SetCookie(w, sessionCookie(id, 3600))
	w.WriteHeader(http.StatusOK)
}

func logoutHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return
	}
	if c, err := r.Cookie("session"); err == nil {
		sessions.Lock()
		delete(sessions.byID, c.Value)
		sessions.Unlock()
	}
	http.SetCookie(w, sessionCookie("", -1))
	w.WriteHeader(http.StatusOK)
}

func userHandler(db *sql.DB, w http.ResponseWriter, r *http.Request) {
	id, err := strconv.Atoi(r.URL.Query().Get("id"))
	if err != nil {
		http.Error(w, "invalid id", http.StatusBadRequest)
		return
	}
	if db == nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	rows, err := db.Query("SELECT name, email FROM users WHERE id = ?", id)
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	defer rows.Close()
	for rows.Next() {
		var name, email string
		if err := rows.Scan(&name, &email); err != nil {
			http.Error(w, "internal error", http.StatusInternalServerError)
			return
		}
		fmt.Fprintf(w, "%s <%s>\n", name, email)
	}
	if err := rows.Err(); err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
	}
}

func fileHandler(w http.ResponseWriter, r *http.Request) {
	name := r.URL.Query().Get("f")
	data, _ := os.ReadFile("/var/data/" + name)
	w.Write(data)
}

func main() {
	http.HandleFunc("/login", loginHandler)
	http.HandleFunc("/logout", logoutHandler)
	http.HandleFunc("/user", func(w http.ResponseWriter, r *http.Request) { userHandler(nil, w, r) })
	http.HandleFunc("/file", fileHandler)
	_ = http.ListenAndServe(":8080", nil)
}
