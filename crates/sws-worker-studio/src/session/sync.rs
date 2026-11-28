//! Pure, host-testable routing and authorization helpers for HTTP `/sync` (#68).

use crate::auth::policy::Permission;
use crate::session::engine::Rejection;
use crate::session::is_valid_id;
use crate::session::protocol::{CheckpointReason, ClientMessage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRoute {
    pub ws: String,
    pub site: String,
}

/// Match `/api/sites/{ws}/{site}/sync`; every id must satisfy the registry
/// grammar, otherwise it is not a route (404).
pub fn parse_route(path: &str) -> Option<SyncRoute> {
    let rest = path.strip_prefix("/api/sites/")?;
    let mut parts = rest.split('/');
    let (ws, site, tail) = (parts.next()?, parts.next()?, parts.next()?);
    if tail != "sync" || parts.next().is_some() {
        return None;
    }
    (is_valid_id(ws) && is_valid_id(site)).then(|| SyncRoute {
        ws: ws.to_string(),
        site: site.to_string(),
    })
}

/// Permission required to execute a `POST /sync` message. Returns `None` for
/// message types that are only valid on WebSockets (`ping`, `sync`, `presence`).
pub fn required_permission(msg: &ClientMessage) -> Option<Permission> {
    match msg {
        ClientMessage::Seed { .. }
        | ClientMessage::Checkpoint {
            reason: CheckpointReason::Publish,
        } => Some(Permission::Publish),
        ClientMessage::Batch { .. }
        | ClientMessage::Undo { .. }
        | ClientMessage::Checkpoint {
            reason: CheckpointReason::Save | CheckpointReason::Auto,
        } => Some(Permission::Edit),
        ClientMessage::Ping
        | ClientMessage::Sync { .. }
        | ClientMessage::PresenceGet
        | ClientMessage::Presence { .. } => None,
    }
}

/// HTTP status code for a session engine rejection.
pub fn rejection_status(r: &Rejection) -> u16 {
    match r {
        Rejection::NotSeeded | Rejection::NotFound(_) => 404,
        Rejection::AlreadySeeded
        | Rejection::OpIdReused
        | Rejection::Conflict(_)
        | Rejection::InvalidBase { .. }
        | Rejection::CheckpointBacklog => 409,
        Rejection::UndoNotOwner => 403,
        Rejection::RateLimited => 429,
        Rejection::Command(_)
        | Rejection::InvalidDocument(_)
        | Rejection::DocumentTooLarge { .. } => 400,
        Rejection::Unavailable(_) => 503,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::protocol::parse_client;

    #[test]
    fn parse_route_validates_ids_strictly() {
        assert_eq!(
            parse_route("/api/sites/ws-a/site_1/sync"),
            Some(SyncRoute {
                ws: "ws-a".into(),
                site: "site_1".into(),
            })
        );
        for bad in [
            "/api/sites/ws-a/site_1/sync/",
            "/api/sites/ws-a/site_1/sync/extra",
            "/api/sites/WS/site_1/sync",
            "/api/sites/ws-a/site.1/sync",
            "/api/sites/ws-a/site_1/session",
        ] {
            assert_eq!(parse_route(bad), None, "{bad}");
        }
    }

    #[test]
    fn post_sync_permission_splits_edit_and_publish() {
        let batch = parse_client(
            r#"{"v":1,"type":"batch","op_id":"op-12345678","base_seq":0,"batch":{"version":1,"commands":[{"op":"set_site_meta","name":"X"}]}}"#,
        )
        .unwrap();
        assert_eq!(required_permission(&batch), Some(Permission::Edit));

        let save = parse_client(r#"{"v":1,"type":"checkpoint","reason":"save"}"#).unwrap();
        assert_eq!(required_permission(&save), Some(Permission::Edit));

        let publish = parse_client(r#"{"v":1,"type":"checkpoint","reason":"publish"}"#).unwrap();
        assert_eq!(required_permission(&publish), Some(Permission::Publish));

        let seed = parse_client(r#"{"v":1,"type":"seed","document":"{}"}"#).unwrap();
        assert_eq!(required_permission(&seed), Some(Permission::Publish));

        let ping = parse_client(r#"{"v":1,"type":"ping"}"#).unwrap();
        assert_eq!(required_permission(&ping), None);
    }
}
