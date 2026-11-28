SESSION_COOKIE = "session"


def cookie_value(request):
    return request.cookies.get(SESSION_COOKIE, "")
