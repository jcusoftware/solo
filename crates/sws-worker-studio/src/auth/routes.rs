//! Routing for the authentication, token and membership endpoints (pure).
//!
//! | Method | Path | Who |
//! | --- | --- | --- |
//! | GET | `/auth/github/start` | anyone (redirects to GitHub) |
//! | GET | `/auth/github/callback` | GitHub redirect |
//! | GET | `/auth/session` | session or token: who am I |
//! | POST | `/auth/logout` | session |
//! | GET | `/invite` | anyone: the invitation form (Turnstile) |
//! | POST | `/auth/invitations/accept` | anyone with a code + Turnstile |
//! | GET | `/api/tokens` | session: my tokens |
//! | POST | `/api/workspaces/{ws}/tokens` | fresh session |
//! | POST | `/api/tokens/{id}/rotate` | fresh session |
//! | DELETE | `/api/tokens/{id}` | fresh session |
//! | DELETE | `/api/tokens/current` | the bearer token itself |
//! | GET | `/api/workspaces/{ws}/members` | any member |
//! | PATCH / DELETE | `/api/workspaces/{ws}/members/{user_id}` | owner, fresh session |
//! | GET / POST | `/api/workspaces/{ws}/invitations` | owner (POST: fresh session) |
//! | DELETE | `/api/workspaces/{ws}/invitations/{id}` | owner, fresh session |

use crate::auth::secret::is_lookup_id;
use crate::bounds::{self, Reject};
use crate::session::is_valid_id;

/// Body cap for every auth route (JSON or the invitation form).
pub const MAX_AUTH_BODY: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthRoute {
    Start,
    Callback,
    Me,
    Logout,
    InvitePage,
    InviteAccept,
    TokensList,
    TokenCreate {
        ws: String,
    },
    TokenRotate {
        id: String,
    },
    TokenRevoke {
        id: String,
    },
    TokenRevokeCurrent,
    MembersList {
        ws: String,
    },
    MemberUpdate {
        ws: String,
        user_id: i64,
    },
    MemberRemove {
        ws: String,
        user_id: i64,
    },
    InvitationsList {
        ws: String,
    },
    InvitationCreate {
        ws: String,
    },
    InvitationRevoke {
        ws: String,
        id: String,
    },
    /// Known path, wrong method (`Allow` value).
    MethodNotAllowed(&'static str),
    /// Under an auth prefix but malformed.
    NotFound,
}

impl AuthRoute {
    /// Routes a browser reaches by top-level navigation (HTML/redirect answers).
    pub fn is_browser_page(&self) -> bool {
        matches!(
            self,
            AuthRoute::Start
                | AuthRoute::Callback
                | AuthRoute::InvitePage
                | AuthRoute::InviteAccept
        )
    }
}

fn user_id(s: &str) -> Option<i64> {
    (s.len() <= 16 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse::<i64>().ok())
        .flatten()
        .filter(|n| *n > 0)
}

fn pick(
    method: &str,
    allow: &'static str,
    ok: impl FnOnce(&str) -> Option<AuthRoute>,
) -> AuthRoute {
    ok(method).unwrap_or(AuthRoute::MethodNotAllowed(allow))
}

/// `None` = not an auth path (the caller keeps routing).
pub fn parse(method: &str, path: &str) -> Option<AuthRoute> {
    use AuthRoute as R;
    let r = match path {
        "/auth/github/start" => pick(method, "GET", |m| (m == "GET").then_some(R::Start)),
        "/auth/github/callback" => pick(method, "GET", |m| (m == "GET").then_some(R::Callback)),
        "/auth/session" => pick(method, "GET", |m| (m == "GET").then_some(R::Me)),
        "/auth/logout" => pick(method, "POST", |m| (m == "POST").then_some(R::Logout)),
        "/invite" => pick(method, "GET", |m| (m == "GET").then_some(R::InvitePage)),
        "/auth/invitations/accept" => {
            pick(method, "POST", |m| (m == "POST").then_some(R::InviteAccept))
        }
        "/api/tokens" => pick(method, "GET", |m| (m == "GET").then_some(R::TokensList)),
        "/api/tokens/current" => pick(method, "DELETE", |m| {
            (m == "DELETE").then_some(R::TokenRevokeCurrent)
        }),
        _ => {
            if let Some(rest) = path.strip_prefix("/api/tokens/") {
                let parts: Vec<&str> = rest.split('/').collect();
                return Some(match parts.as_slice() {
                    [id] if is_lookup_id(id) => pick(method, "DELETE", |m| {
                        (m == "DELETE").then(|| R::TokenRevoke { id: id.to_string() })
                    }),
                    [id, "rotate"] if is_lookup_id(id) => pick(method, "POST", |m| {
                        (m == "POST").then(|| R::TokenRotate { id: id.to_string() })
                    }),
                    _ => R::NotFound,
                });
            }
            let rest = path.strip_prefix("/api/workspaces/")?;
            let parts: Vec<&str> = rest.split('/').collect();
            let ws = |w: &str| w.to_string();
            return match parts.as_slice() {
                [w, "tokens"] if is_valid_id(w) => Some(pick(method, "POST", |m| {
                    (m == "POST").then(|| R::TokenCreate { ws: ws(w) })
                })),
                [w, "members"] if is_valid_id(w) => Some(pick(method, "GET", |m| {
                    (m == "GET").then(|| R::MembersList { ws: ws(w) })
                })),
                [w, "members", u] if is_valid_id(w) => Some(match user_id(u) {
                    Some(user_id) => pick(method, "PATCH, DELETE", |m| match m {
                        "PATCH" => Some(R::MemberUpdate { ws: ws(w), user_id }),
                        "DELETE" => Some(R::MemberRemove { ws: ws(w), user_id }),
                        _ => None,
                    }),
                    None => R::NotFound,
                }),
                [w, "invitations"] if is_valid_id(w) => {
                    Some(pick(method, "GET, POST", |m| match m {
                        "GET" => Some(R::InvitationsList { ws: ws(w) }),
                        "POST" => Some(R::InvitationCreate { ws: ws(w) }),
                        _ => None,
                    }))
                }
                [w, "invitations", id] if is_valid_id(w) && is_lookup_id(id) => {
                    Some(pick(method, "DELETE", |m| {
                        (m == "DELETE").then(|| R::InvitationRevoke {
                            ws: ws(w),
                            id: id.to_string(),
                        })
                    }))
                }
                // Not ours: other /api/workspaces/... paths keep routing (404 there).
                _ => None,
            };
        }
    };
    Some(r)
}

/// What body a request carries after framing checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    None,
    Json,
    /// `application/x-www-form-urlencoded` (only the invitation form).
    Form,
}

fn is_form(ct: Option<&str>) -> bool {
    let Some(ct) = ct else { return false };
    let mut parts = ct.split(';');
    parts.next().is_some_and(|m| {
        m.trim()
            .eq_ignore_ascii_case("application/x-www-form-urlencoded")
    }) && parts.all(|p| match p.split_once('=') {
        Some((k, v)) if k.trim().eq_ignore_ascii_case("charset") => {
            let cs = v.trim().trim_matches('"');
            cs.eq_ignore_ascii_case("utf-8") || cs.eq_ignore_ascii_case("utf8")
        }
        _ => p.trim().is_empty(),
    })
}

/// Framing for auth routes, before any body byte is read: the standard bounds
/// (JSON only, `Content-Length` required on body methods, no
/// `Transfer-Encoding`), the tighter [`MAX_AUTH_BODY`] cap, and the invitation
/// form's urlencoded body as the one non-JSON exception.
pub fn check_framing(
    route: &AuthRoute,
    method: &str,
    url_len: usize,
    content_length: Option<&str>,
    transfer_encoding: Option<&str>,
    content_type: Option<&str>,
) -> Result<BodyKind, Reject> {
    let declared = |cl: Option<&str>| -> Result<usize, Reject> {
        let raw = cl.ok_or(Reject::LengthRequired)?.trim();
        if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Reject::BadRequest);
        }
        let n = raw.parse::<usize>().map_err(|_| Reject::PayloadTooLarge)?;
        if n > MAX_AUTH_BODY {
            return Err(Reject::PayloadTooLarge);
        }
        Ok(n)
    };
    if *route == AuthRoute::InviteAccept && method == "POST" {
        if url_len > bounds::MAX_URL_BYTES {
            return Err(Reject::UriTooLong);
        }
        if transfer_encoding.is_some() {
            return Err(Reject::LengthRequired);
        }
        let n = declared(content_length)?;
        if n == 0 {
            return Err(Reject::BadRequest);
        }
        return if is_form(content_type) {
            Ok(BodyKind::Form)
        } else {
            Err(Reject::UnsupportedMediaType)
        };
    }
    bounds::check_request(
        method,
        url_len,
        content_length,
        transfer_encoding,
        content_type,
    )?;
    if matches!(method, "POST" | "PUT" | "PATCH") && declared(content_length)? > 0 {
        return Ok(BodyKind::Json);
    }
    Ok(BodyKind::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef01234567";

    #[test]
    fn auth_routes() {
        assert_eq!(parse("GET", "/auth/github/start"), Some(AuthRoute::Start));
        assert_eq!(
            parse("GET", "/auth/github/callback"),
            Some(AuthRoute::Callback)
        );
        assert_eq!(parse("POST", "/auth/logout"), Some(AuthRoute::Logout));
        assert_eq!(
            parse("GET", "/auth/logout"),
            Some(AuthRoute::MethodNotAllowed("POST")),
            "logout is never a GET (no CSRF by link)"
        );
        assert_eq!(parse("GET", "/invite"), Some(AuthRoute::InvitePage));
        assert_eq!(
            parse("POST", "/auth/invitations/accept"),
            Some(AuthRoute::InviteAccept)
        );
        assert_eq!(parse("GET", "/auth/session"), Some(AuthRoute::Me));
        assert_eq!(parse("GET", "/api/health"), None);
        assert_eq!(parse("GET", "/api/sites/ws/s/session"), None);
    }

    #[test]
    fn token_routes() {
        assert_eq!(parse("GET", "/api/tokens"), Some(AuthRoute::TokensList));
        assert_eq!(
            parse("POST", "/api/workspaces/ws-a/tokens"),
            Some(AuthRoute::TokenCreate { ws: "ws-a".into() })
        );
        assert_eq!(
            parse("POST", &format!("/api/tokens/{ID}/rotate")),
            Some(AuthRoute::TokenRotate { id: ID.into() })
        );
        assert_eq!(
            parse("DELETE", &format!("/api/tokens/{ID}")),
            Some(AuthRoute::TokenRevoke { id: ID.into() })
        );
        assert_eq!(
            parse("DELETE", "/api/tokens/current"),
            Some(AuthRoute::TokenRevokeCurrent)
        );
        assert_eq!(
            parse("DELETE", "/api/tokens/nope"),
            Some(AuthRoute::NotFound)
        );
        assert_eq!(
            parse("GET", &format!("/api/tokens/{ID}")),
            Some(AuthRoute::MethodNotAllowed("DELETE"))
        );
    }

    #[test]
    fn member_routes_parse_ids_strictly() {
        assert_eq!(
            parse("PATCH", "/api/workspaces/ws-a/members/12"),
            Some(AuthRoute::MemberUpdate {
                ws: "ws-a".into(),
                user_id: 12
            })
        );
        assert_eq!(
            parse("DELETE", "/api/workspaces/ws-a/members/12"),
            Some(AuthRoute::MemberRemove {
                ws: "ws-a".into(),
                user_id: 12
            })
        );
        for bad in ["0", "-1", "1e3", "", "99999999999999999999", "+1"] {
            assert_eq!(
                parse("DELETE", &format!("/api/workspaces/ws-a/members/{bad}")),
                Some(AuthRoute::NotFound),
                "{bad}"
            );
        }
        assert_eq!(parse("GET", "/api/workspaces/WS/members"), None);
        assert_eq!(parse("GET", "/api/workspaces/ws-a/other"), None);
        assert_eq!(
            parse("PUT", "/api/workspaces/ws-a/invitations"),
            Some(AuthRoute::MethodNotAllowed("GET, POST"))
        );
        assert_eq!(
            parse("DELETE", &format!("/api/workspaces/ws-a/invitations/{ID}")),
            Some(AuthRoute::InvitationRevoke {
                ws: "ws-a".into(),
                id: ID.into()
            })
        );
        assert!(AuthRoute::Callback.is_browser_page());
    }

    #[test]
    fn framing() {
        let form = Some("application/x-www-form-urlencoded");
        let json = Some("application/json");
        let accept = AuthRoute::InviteAccept;
        assert_eq!(
            check_framing(&accept, "POST", 30, Some("10"), None, form),
            Ok(BodyKind::Form)
        );
        assert_eq!(
            check_framing(
                &accept,
                "POST",
                30,
                Some("10"),
                None,
                Some("application/x-www-form-urlencoded; charset=UTF-8")
            ),
            Ok(BodyKind::Form)
        );
        assert_eq!(
            check_framing(&accept, "POST", 30, Some("10"), None, json),
            Err(Reject::UnsupportedMediaType)
        );
        assert_eq!(
            check_framing(&accept, "POST", 30, None, None, form),
            Err(Reject::LengthRequired)
        );
        assert_eq!(
            check_framing(&accept, "POST", 30, Some("10"), Some("chunked"), form),
            Err(Reject::LengthRequired)
        );
        assert_eq!(
            check_framing(&accept, "POST", 30, Some("0"), None, form),
            Err(Reject::BadRequest)
        );
        assert_eq!(
            check_framing(&accept, "POST", 30, Some("4097"), None, form),
            Err(Reject::PayloadTooLarge)
        );
        let create = AuthRoute::TokenCreate { ws: "w".into() };
        assert_eq!(
            check_framing(&create, "POST", 30, Some("20"), None, json),
            Ok(BodyKind::Json)
        );
        assert_eq!(
            check_framing(&create, "POST", 30, Some("20"), None, form),
            Err(Reject::UnsupportedMediaType)
        );
        assert_eq!(
            check_framing(&create, "POST", 30, Some("5000"), None, json),
            Err(Reject::PayloadTooLarge)
        );
        assert_eq!(
            check_framing(&AuthRoute::Logout, "POST", 30, Some("0"), None, None),
            Ok(BodyKind::None)
        );
        assert_eq!(
            check_framing(&AuthRoute::Me, "GET", 30, None, None, None),
            Ok(BodyKind::None)
        );
        assert_eq!(
            check_framing(&AuthRoute::Me, "GET", 30, Some("3"), None, None),
            Err(Reject::BadRequest)
        );
        assert!(!AuthRoute::Logout.is_browser_page());
    }
}
